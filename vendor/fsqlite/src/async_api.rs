//! Async-native wrapper around [`Connection`] for use with asupersync's `Cx` capability context.
//!
//! Because [`Connection`] is `!Send` (it uses `Rc<RefCell<..>>` internally), this module
//! provides an [`AsyncConnection`] that spawns a dedicated worker thread owning the
//! `Connection`. All SQL operations are dispatched to the worker via a command channel
//! and results are returned through oneshot channels.
//!
//! Every async method accepts a `&Cx` and calls [`Cx::checkpoint()`] before dispatching,
//! ensuring cancel-correctness: if the context has been cancelled, the operation fails
//! fast without blocking on the worker.
//!
//! # Feature gate
//!
//! This module is only available when the `async-api` feature is enabled on `fsqlite`.
//!
//! # Example
//!
//! ```ignore
//! use fsqlite::{AsyncConnection, SqliteValue};
//! use fsqlite_types::cx::Cx;
//!
//! async fn example(cx: &Cx) -> Result<(), fsqlite::FrankenError> {
//!     let conn = AsyncConnection::open(cx, ":memory:").await?;
//!     conn.execute(cx, "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)").await?;
//!     conn.execute_with_params(
//!         cx,
//!         "INSERT INTO t VALUES (?1, ?2)",
//!         &[SqliteValue::Integer(1), SqliteValue::Text("hello".into())],
//!     ).await?;
//!     let rows = conn.query(cx, "SELECT * FROM t").await?;
//!     assert_eq!(rows.len(), 1);
//!     Ok(())
//! }
//! ```

use crate::{Connection, ConnectionEnv, FrankenError, Row, SqliteValue};
use fsqlite_types::cx::Cx;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

// ---------------------------------------------------------------------------
// Command protocol between async methods and the worker thread
// ---------------------------------------------------------------------------

type Responder<T> = std::sync::mpsc::SyncSender<Result<T, FrankenError>>;

/// A command sent from an async method to the worker thread.
enum Command {
    Query {
        sql: String,
        tx: Responder<Vec<Row>>,
    },
    QueryWithParams {
        sql: String,
        params: Vec<SqliteValue>,
        tx: Responder<Vec<Row>>,
    },
    QueryRow {
        sql: String,
        tx: Responder<Row>,
    },
    QueryRowWithParams {
        sql: String,
        params: Vec<SqliteValue>,
        tx: Responder<Row>,
    },
    Execute {
        sql: String,
        tx: Responder<usize>,
    },
    ExecuteWithParams {
        sql: String,
        params: Vec<SqliteValue>,
        tx: Responder<usize>,
    },
    ExecuteBatch {
        sql: String,
        tx: Responder<()>,
    },
    BeginTransaction {
        tx: Responder<()>,
    },
    CommitTransaction {
        tx: Responder<()>,
    },
    RollbackTransaction {
        tx: Responder<()>,
    },
    Close {
        tx: Responder<()>,
    },
    Shutdown,
}

// ---------------------------------------------------------------------------
// Oneshot future for receiving from std::sync::mpsc in an async context
// ---------------------------------------------------------------------------

/// A `Future` that polls an `mpsc::Receiver` for a single value.
///
/// Uses `try_recv` to avoid blocking the async runtime. When the value is not
/// yet available, a single background waker thread is spawned that blocks on
/// the receiver and wakes the task when the value arrives. Subsequent polls
/// before the value is ready simply update the stored waker (in case the
/// executor migrated the task to a new waker) without spawning additional
/// threads.
struct OneshotFuture<T> {
    rx: Option<mpsc::Receiver<T>>,
    /// Shared waker slot: the waker thread reads from this so it always wakes
    /// the most-recently-registered waker (handles executor waker rotation).
    /// `None` before the first `Empty` poll; `Some` once the waker thread has
    /// been spawned.
    shared_waker: Option<Arc<std::sync::Mutex<Option<std::task::Waker>>>>,
}

impl<T> OneshotFuture<T> {
    fn new(rx: mpsc::Receiver<T>) -> Self {
        Self {
            rx: Some(rx),
            shared_waker: None,
        }
    }
}

impl<T> std::future::Future for OneshotFuture<T>
where
    T: Send + 'static,
{
    type Output = Result<T, FrankenError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        task_cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.get_mut();
        let rx = this.rx.as_ref().expect("polled after completion");

        match rx.try_recv() {
            Ok(val) => {
                this.rx = None;
                std::task::Poll::Ready(Ok(val))
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                this.rx = None;
                std::task::Poll::Ready(Err(FrankenError::Internal(
                    "async worker thread terminated unexpectedly".to_owned(),
                )))
            }
            Err(mpsc::TryRecvError::Empty) => {
                if let Some(ref shared) = this.shared_waker {
                    // Waker thread already running — just update the waker in
                    // case the executor rotated it (common with work-stealing
                    // schedulers).
                    let mut guard = shared.lock().expect("waker mutex poisoned");
                    *guard = Some(task_cx.waker().clone());
                } else {
                    // First time we see Empty — spawn one helper thread that
                    // blocks on the receiver and wakes the task when the value
                    // arrives or the sender disconnects.
                    let shared = Arc::new(std::sync::Mutex::new(Some(task_cx.waker().clone())));
                    this.shared_waker = Some(Arc::clone(&shared));

                    let rx_taken = this.rx.take().unwrap();
                    let (ready_tx, ready_rx) = mpsc::sync_channel::<T>(1);

                    thread::Builder::new()
                        .name("fsqlite-async-waker".into())
                        .spawn(move || {
                            // Block until the worker sends the result.
                            match rx_taken.recv() {
                                Ok(val) => {
                                    let _ = ready_tx.send(val);
                                }
                                Err(_) => {
                                    // Sender dropped — will surface as
                                    // Disconnected on next poll.
                                }
                            }
                            // Wake the latest waker (may differ from the one
                            // captured at spawn time if the executor rotated).
                            if let Some(w) = shared.lock().expect("waker mutex poisoned").take() {
                                w.wake();
                            }
                        })
                        .expect("failed to spawn async waker thread");

                    // Replace our receiver with the forwarding channel so the
                    // next poll reads from it.
                    this.rx = Some(ready_rx);
                }
                std::task::Poll::Pending
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Worker thread
// ---------------------------------------------------------------------------

fn worker_loop(mut conn: Connection, rx: mpsc::Receiver<Command>) {
    while let Ok(cmd) = rx.recv() {
        match cmd {
            Command::Query { sql, tx } => {
                let _ = tx.send(conn.query(&sql));
            }
            Command::QueryWithParams { sql, params, tx } => {
                let _ = tx.send(conn.query_with_params(&sql, &params));
            }
            Command::QueryRow { sql, tx } => {
                let _ = tx.send(conn.query_row(&sql));
            }
            Command::QueryRowWithParams { sql, params, tx } => {
                let _ = tx.send(conn.query_row_with_params(&sql, &params));
            }
            Command::Execute { sql, tx } => {
                let _ = tx.send(conn.execute(&sql));
            }
            Command::ExecuteWithParams { sql, params, tx } => {
                let _ = tx.send(conn.execute_with_params(&sql, &params));
            }
            Command::ExecuteBatch { sql, tx } => {
                let _ = tx.send(conn.execute_batch(&sql));
            }
            Command::BeginTransaction { tx } => {
                let _ = tx.send(conn.begin_transaction());
            }
            Command::CommitTransaction { tx } => {
                let _ = tx.send(conn.commit_transaction());
            }
            Command::RollbackTransaction { tx } => {
                let _ = tx.send(conn.rollback_transaction());
            }
            Command::Close { tx } => {
                // Close the connection explicitly (rolls back any active txn,
                // runs a passive WAL checkpoint).
                let _ = tx.send(conn.close_in_place());
                return;
            }
            Command::Shutdown => {
                return;
            }
        }
    }
    // Channel closed — connection will be dropped here.
}

// ---------------------------------------------------------------------------
// Cx → FrankenError bridge
// ---------------------------------------------------------------------------

/// Map a `Cx::checkpoint()` cancellation error to a `FrankenError::Interrupt`.
fn checkpoint_or_interrupt<Caps: fsqlite_types::cx::cap::SubsetOf<fsqlite_types::cx::cap::All>>(
    cx: &Cx<Caps>,
) -> Result<(), FrankenError> {
    cx.checkpoint().map_err(|_| FrankenError::Interrupt)
}

/// Map a send error (worker died) to a `FrankenError::Internal`.
fn send_err<T>(_: mpsc::SendError<T>) -> FrankenError {
    FrankenError::Internal("async worker thread is no longer running".to_owned())
}

// ---------------------------------------------------------------------------
// AsyncConnection
// ---------------------------------------------------------------------------

/// Async-native wrapper around [`Connection`] for use with asupersync's `Cx`
/// capability context.
///
/// All methods accept a `&Cx` and call `cx.checkpoint()` before dispatching,
/// providing structural cancel-correctness. If the context is cancelled, the
/// method returns `FrankenError::Interrupt` immediately without touching the
/// underlying connection.
///
/// The connection itself lives on a dedicated worker thread (because
/// [`Connection`] is `!Send`). Commands are dispatched via an internal channel
/// and results flow back through oneshot channels.
///
/// # Shutdown
///
/// When `AsyncConnection` is dropped, the worker thread is signalled to shut
/// down. The underlying [`Connection`] is closed on the worker thread as part
/// of its normal drop sequence.
///
/// For explicit, error-checked shutdown use [`close`](Self::close).
pub struct AsyncConnection {
    cmd_tx: Option<mpsc::SyncSender<Command>>,
    worker: Option<thread::JoinHandle<()>>,
    /// Tracks whether the worker thread's connection has an active transaction.
    /// Updated by `begin_transaction`, `commit_transaction`, and
    /// `rollback_transaction` to allow `in_transaction()` to be a cheap local
    /// read without a round-trip to the worker.
    in_txn: Arc<AtomicBool>,
}

impl AsyncConnection {
    /// Open a database connection asynchronously with `Cx` integration.
    ///
    /// The `Cx` is checkpointed before the blocking open. On success, a
    /// dedicated worker thread is spawned to own the `Connection`.
    pub async fn open<Caps>(cx: &Cx<Caps>, path: impl Into<String>) -> Result<Self, FrankenError>
    where
        Caps: fsqlite_types::cx::cap::SubsetOf<fsqlite_types::cx::cap::All>,
    {
        Self::open_with_env(cx, path, ConnectionEnv::default()).await
    }

    /// Open a database connection without a capability context (convenience).
    ///
    /// Equivalent to calling [`Connection::open`] and wrapping the result.
    /// No cancellation check is performed.
    pub fn open_sync(path: impl Into<String>) -> Result<Self, FrankenError> {
        Self::open_sync_with_env(path, ConnectionEnv::default())
    }

    /// Open a database connection without a capability context, with a custom
    /// [`ConnectionEnv`].
    pub fn open_sync_with_env(
        path: impl Into<String>,
        env: ConnectionEnv,
    ) -> Result<Self, FrankenError> {
        let path = path.into();
        let (open_tx, open_rx) = mpsc::sync_channel::<Result<(), FrankenError>>(1);
        let (cmd_tx, cmd_rx) = mpsc::sync_channel::<Command>(32);

        let thread_name = format!("fsqlite-async-worker:{path}");
        let worker = thread::Builder::new()
            .name(thread_name)
            .spawn(move || match Connection::open_with_env(path, env) {
                Ok(conn) => {
                    let _ = open_tx.send(Ok(()));
                    worker_loop(conn, cmd_rx);
                }
                Err(e) => {
                    let _ = open_tx.send(Err(e));
                }
            })
            .map_err(|e| FrankenError::Internal(format!("failed to spawn worker thread: {e}")))?;

        // Block on the open result (sync path, no async executor needed).
        open_rx
            .recv()
            .map_err(|_| FrankenError::Internal("worker thread terminated during open".to_owned()))?
            .map(|()| Self {
                cmd_tx: Some(cmd_tx),
                worker: Some(worker),
                in_txn: Arc::new(AtomicBool::new(false)),
            })
    }

    /// Open a database connection with an explicit [`ConnectionEnv`].
    pub async fn open_with_env<Caps>(
        cx: &Cx<Caps>,
        path: impl Into<String>,
        env: ConnectionEnv,
    ) -> Result<Self, FrankenError>
    where
        Caps: fsqlite_types::cx::cap::SubsetOf<fsqlite_types::cx::cap::All>,
    {
        checkpoint_or_interrupt(cx)?;

        let path = path.into();

        // Open the connection on a background thread (it is !Send, so it
        // must be born on and stay on the worker thread).
        let (open_tx, open_rx) = mpsc::sync_channel::<Result<(), FrankenError>>(1);
        let (cmd_tx, cmd_rx) = mpsc::sync_channel::<Command>(32);

        let thread_name = format!("fsqlite-async-worker:{path}");
        let worker = thread::Builder::new()
            .name(thread_name)
            .spawn(move || match Connection::open_with_env(path, env) {
                Ok(conn) => {
                    let _ = open_tx.send(Ok(()));
                    worker_loop(conn, cmd_rx);
                }
                Err(e) => {
                    let _ = open_tx.send(Err(e));
                }
            })
            .map_err(|e| FrankenError::Internal(format!("failed to spawn worker thread: {e}")))?;

        // Wait for the open result.
        let result = OneshotFuture::new(open_rx).await?;
        result?;

        Ok(Self {
            cmd_tx: Some(cmd_tx),
            worker: Some(worker),
            in_txn: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Return a reference to the command sender, or an error if the worker is gone.
    fn sender(&self) -> Result<&mpsc::SyncSender<Command>, FrankenError> {
        self.cmd_tx
            .as_ref()
            .ok_or_else(|| FrankenError::Internal("AsyncConnection has been closed".to_owned()))
    }

    /// Execute a SQL query and return all result rows.
    pub async fn query<Caps>(&self, cx: &Cx<Caps>, sql: &str) -> Result<Vec<Row>, FrankenError>
    where
        Caps: fsqlite_types::cx::cap::SubsetOf<fsqlite_types::cx::cap::All>,
    {
        checkpoint_or_interrupt(cx)?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender()?
            .send(Command::Query {
                sql: sql.to_owned(),
                tx,
            })
            .map_err(send_err)?;
        OneshotFuture::new(rx).await?
    }

    /// Execute a query with bound parameters and return all result rows.
    pub async fn query_with_params<Caps>(
        &self,
        cx: &Cx<Caps>,
        sql: &str,
        params: &[SqliteValue],
    ) -> Result<Vec<Row>, FrankenError>
    where
        Caps: fsqlite_types::cx::cap::SubsetOf<fsqlite_types::cx::cap::All>,
    {
        checkpoint_or_interrupt(cx)?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender()?
            .send(Command::QueryWithParams {
                sql: sql.to_owned(),
                params: params.to_vec(),
                tx,
            })
            .map_err(send_err)?;
        OneshotFuture::new(rx).await?
    }

    /// Execute a query and return exactly one row.
    pub async fn query_row<Caps>(&self, cx: &Cx<Caps>, sql: &str) -> Result<Row, FrankenError>
    where
        Caps: fsqlite_types::cx::cap::SubsetOf<fsqlite_types::cx::cap::All>,
    {
        checkpoint_or_interrupt(cx)?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender()?
            .send(Command::QueryRow {
                sql: sql.to_owned(),
                tx,
            })
            .map_err(send_err)?;
        OneshotFuture::new(rx).await?
    }

    /// Execute a query with parameters and return exactly one row.
    pub async fn query_row_with_params<Caps>(
        &self,
        cx: &Cx<Caps>,
        sql: &str,
        params: &[SqliteValue],
    ) -> Result<Row, FrankenError>
    where
        Caps: fsqlite_types::cx::cap::SubsetOf<fsqlite_types::cx::cap::All>,
    {
        checkpoint_or_interrupt(cx)?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender()?
            .send(Command::QueryRowWithParams {
                sql: sql.to_owned(),
                params: params.to_vec(),
                tx,
            })
            .map_err(send_err)?;
        OneshotFuture::new(rx).await?
    }

    /// Execute SQL and return the number of affected/output rows.
    pub async fn execute<Caps>(&self, cx: &Cx<Caps>, sql: &str) -> Result<usize, FrankenError>
    where
        Caps: fsqlite_types::cx::cap::SubsetOf<fsqlite_types::cx::cap::All>,
    {
        checkpoint_or_interrupt(cx)?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender()?
            .send(Command::Execute {
                sql: sql.to_owned(),
                tx,
            })
            .map_err(send_err)?;
        OneshotFuture::new(rx).await?
    }

    /// Execute SQL with bound parameters and return the number of affected/output rows.
    pub async fn execute_with_params<Caps>(
        &self,
        cx: &Cx<Caps>,
        sql: &str,
        params: &[SqliteValue],
    ) -> Result<usize, FrankenError>
    where
        Caps: fsqlite_types::cx::cap::SubsetOf<fsqlite_types::cx::cap::All>,
    {
        checkpoint_or_interrupt(cx)?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender()?
            .send(Command::ExecuteWithParams {
                sql: sql.to_owned(),
                params: params.to_vec(),
                tx,
            })
            .map_err(send_err)?;
        OneshotFuture::new(rx).await?
    }

    /// Execute zero or more SQL statements separated by semicolons.
    pub async fn execute_batch<Caps>(&self, cx: &Cx<Caps>, sql: &str) -> Result<(), FrankenError>
    where
        Caps: fsqlite_types::cx::cap::SubsetOf<fsqlite_types::cx::cap::All>,
    {
        checkpoint_or_interrupt(cx)?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender()?
            .send(Command::ExecuteBatch {
                sql: sql.to_owned(),
                tx,
            })
            .map_err(send_err)?;
        OneshotFuture::new(rx).await?
    }

    /// Begin a transaction.
    pub async fn begin_transaction<Caps>(&self, cx: &Cx<Caps>) -> Result<(), FrankenError>
    where
        Caps: fsqlite_types::cx::cap::SubsetOf<fsqlite_types::cx::cap::All>,
    {
        checkpoint_or_interrupt(cx)?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender()?
            .send(Command::BeginTransaction { tx })
            .map_err(send_err)?;
        let result: Result<(), FrankenError> = OneshotFuture::new(rx).await?;
        if result.is_ok() {
            self.in_txn.store(true, Ordering::Release);
        }
        result
    }

    /// Commit the active transaction.
    pub async fn commit_transaction<Caps>(&self, cx: &Cx<Caps>) -> Result<(), FrankenError>
    where
        Caps: fsqlite_types::cx::cap::SubsetOf<fsqlite_types::cx::cap::All>,
    {
        checkpoint_or_interrupt(cx)?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender()?
            .send(Command::CommitTransaction { tx })
            .map_err(send_err)?;
        let result: Result<(), FrankenError> = OneshotFuture::new(rx).await?;
        if result.is_ok() {
            self.in_txn.store(false, Ordering::Release);
        }
        result
    }

    /// Roll back the active transaction.
    pub async fn rollback_transaction<Caps>(&self, cx: &Cx<Caps>) -> Result<(), FrankenError>
    where
        Caps: fsqlite_types::cx::cap::SubsetOf<fsqlite_types::cx::cap::All>,
    {
        checkpoint_or_interrupt(cx)?;
        let (tx, rx) = mpsc::sync_channel(1);
        self.sender()?
            .send(Command::RollbackTransaction { tx })
            .map_err(send_err)?;
        let result: Result<(), FrankenError> = OneshotFuture::new(rx).await?;
        if result.is_ok() {
            self.in_txn.store(false, Ordering::Release);
        }
        result
    }

    /// Returns `true` if an explicit transaction is currently active.
    ///
    /// This is a cheap local read — no round-trip to the worker thread.
    #[must_use]
    pub fn in_transaction(&self) -> bool {
        self.in_txn.load(Ordering::Acquire)
    }

    /// Explicitly close the connection, returning any error from the close operation.
    ///
    /// After this call, all subsequent operations will return an error.
    /// The worker thread is joined before returning.
    pub async fn close<Caps>(&mut self, cx: &Cx<Caps>) -> Result<(), FrankenError>
    where
        Caps: fsqlite_types::cx::cap::SubsetOf<fsqlite_types::cx::cap::All>,
    {
        checkpoint_or_interrupt(cx)?;

        if let Some(cmd_tx) = self.cmd_tx.take() {
            let (tx, rx) = mpsc::sync_channel(1);
            cmd_tx.send(Command::Close { tx }).map_err(send_err)?;
            let result = OneshotFuture::new(rx).await?;

            // Join the worker thread.
            if let Some(handle) = self.worker.take() {
                let _ = handle.join();
            }

            result
        } else {
            // Already closed.
            Ok(())
        }
    }
}

impl Drop for AsyncConnection {
    fn drop(&mut self) {
        // Signal the worker to shut down.
        if let Some(cmd_tx) = self.cmd_tx.take() {
            let _ = cmd_tx.send(Command::Shutdown);
        }
        // Wait for the worker to finish (best-effort, don't block indefinitely).
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fsqlite_types::cx::Cx;

    /// Minimal runtime-free test: verify that open + basic queries work
    /// through the async wrapper using a simple block_on executor.
    #[test]
    fn test_async_connection_basic() {
        fn block_on<F: std::future::Future>(f: F) -> F::Output {
            use std::pin::pin;
            use std::task::{Context, Poll, Wake, Waker};

            struct ThreadWaker;
            impl Wake for ThreadWaker {
                fn wake(self: std::sync::Arc<Self>) {}
            }

            let waker = Waker::from(std::sync::Arc::new(ThreadWaker));
            let mut cx = Context::from_waker(&waker);
            let mut fut = pin!(f);

            loop {
                match fut.as_mut().poll(&mut cx) {
                    Poll::Ready(val) => return val,
                    Poll::Pending => std::thread::yield_now(),
                }
            }
        }

        block_on(async {
            let cx = Cx::new();
            let conn = AsyncConnection::open(&cx, ":memory:")
                .await
                .expect("open should succeed");

            conn.execute(&cx, "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
                .await
                .expect("create table should succeed");

            conn.execute_with_params(
                &cx,
                "INSERT INTO t VALUES (?1, ?2)",
                &[SqliteValue::Integer(1), SqliteValue::Text("hello".into())],
            )
            .await
            .expect("insert should succeed");

            let rows = conn
                .query(&cx, "SELECT * FROM t")
                .await
                .expect("query should succeed");
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].get(0), Some(&SqliteValue::Integer(1)));
            assert_eq!(rows[0].get(1), Some(&SqliteValue::Text("hello".into())));

            let row = conn
                .query_row(&cx, "SELECT name FROM t WHERE id = 1")
                .await
                .expect("query_row should succeed");
            assert_eq!(row.get(0), Some(&SqliteValue::Text("hello".into())));

            let count = conn
                .execute(&cx, "DELETE FROM t")
                .await
                .expect("delete should succeed");
            assert_eq!(count, 1);
        });
    }

    #[test]
    fn test_async_connection_transaction() {
        fn block_on<F: std::future::Future>(f: F) -> F::Output {
            use std::pin::pin;
            use std::task::{Context, Poll, Wake, Waker};

            struct ThreadWaker;
            impl Wake for ThreadWaker {
                fn wake(self: std::sync::Arc<Self>) {}
            }

            let waker = Waker::from(std::sync::Arc::new(ThreadWaker));
            let mut cx = Context::from_waker(&waker);
            let mut fut = pin!(f);

            loop {
                match fut.as_mut().poll(&mut cx) {
                    Poll::Ready(val) => return val,
                    Poll::Pending => std::thread::yield_now(),
                }
            }
        }

        block_on(async {
            let cx = Cx::new();
            let conn = AsyncConnection::open(&cx, ":memory:")
                .await
                .expect("open should succeed");

            conn.execute(&cx, "CREATE TABLE t (id INTEGER PRIMARY KEY)")
                .await
                .expect("create should succeed");

            // Begin, insert, rollback — row should not persist.
            conn.begin_transaction(&cx).await.expect("begin");
            conn.execute(&cx, "INSERT INTO t VALUES (1)")
                .await
                .expect("insert");
            conn.rollback_transaction(&cx).await.expect("rollback");

            let rows = conn.query(&cx, "SELECT * FROM t").await.expect("query");
            assert!(rows.is_empty(), "rollback should have removed the row");

            // Begin, insert, commit — row should persist.
            conn.begin_transaction(&cx).await.expect("begin");
            conn.execute(&cx, "INSERT INTO t VALUES (2)")
                .await
                .expect("insert");
            conn.commit_transaction(&cx).await.expect("commit");

            let rows = conn.query(&cx, "SELECT * FROM t").await.expect("query");
            assert_eq!(rows.len(), 1);
        });
    }

    #[test]
    fn test_async_connection_cancel() {
        fn block_on<F: std::future::Future>(f: F) -> F::Output {
            use std::pin::pin;
            use std::task::{Context, Poll, Wake, Waker};

            struct ThreadWaker;
            impl Wake for ThreadWaker {
                fn wake(self: std::sync::Arc<Self>) {}
            }

            let waker = Waker::from(std::sync::Arc::new(ThreadWaker));
            let mut cx = Context::from_waker(&waker);
            let mut fut = pin!(f);

            loop {
                match fut.as_mut().poll(&mut cx) {
                    Poll::Ready(val) => return val,
                    Poll::Pending => std::thread::yield_now(),
                }
            }
        }

        block_on(async {
            let cx = Cx::new();
            let conn = AsyncConnection::open(&cx, ":memory:")
                .await
                .expect("open should succeed");

            // Cancel the context — subsequent operations should fail.
            cx.cancel();

            let result = conn.execute(&cx, "SELECT 1").await;
            assert!(result.is_err(), "operation should fail after cancellation");
            match result.unwrap_err() {
                FrankenError::Interrupt => {}
                other => panic!("expected Interrupt, got: {other}"),
            }
        });
    }

    #[test]
    fn test_async_connection_execute_batch() {
        fn block_on<F: std::future::Future>(f: F) -> F::Output {
            use std::pin::pin;
            use std::task::{Context, Poll, Wake, Waker};

            struct ThreadWaker;
            impl Wake for ThreadWaker {
                fn wake(self: std::sync::Arc<Self>) {}
            }

            let waker = Waker::from(std::sync::Arc::new(ThreadWaker));
            let mut cx = Context::from_waker(&waker);
            let mut fut = pin!(f);

            loop {
                match fut.as_mut().poll(&mut cx) {
                    Poll::Ready(val) => return val,
                    Poll::Pending => std::thread::yield_now(),
                }
            }
        }

        block_on(async {
            let cx = Cx::new();
            let conn = AsyncConnection::open(&cx, ":memory:")
                .await
                .expect("open should succeed");

            conn.execute_batch(&cx, "CREATE TABLE a (x INTEGER); CREATE TABLE b (y TEXT);")
                .await
                .expect("batch should succeed");

            // Verify both tables exist.
            let _ = conn.query(&cx, "SELECT * FROM a").await.expect("table a");
            let _ = conn.query(&cx, "SELECT * FROM b").await.expect("table b");
        });
    }

    #[test]
    fn test_async_connection_close() {
        fn block_on<F: std::future::Future>(f: F) -> F::Output {
            use std::pin::pin;
            use std::task::{Context, Poll, Wake, Waker};

            struct ThreadWaker;
            impl Wake for ThreadWaker {
                fn wake(self: std::sync::Arc<Self>) {}
            }

            let waker = Waker::from(std::sync::Arc::new(ThreadWaker));
            let mut cx = Context::from_waker(&waker);
            let mut fut = pin!(f);

            loop {
                match fut.as_mut().poll(&mut cx) {
                    Poll::Ready(val) => return val,
                    Poll::Pending => std::thread::yield_now(),
                }
            }
        }

        block_on(async {
            let cx = Cx::new();
            let mut conn = AsyncConnection::open(&cx, ":memory:")
                .await
                .expect("open should succeed");

            conn.close(&cx).await.expect("close should succeed");

            // After close, operations should fail.
            let result = conn.query(&cx, "SELECT 1").await;
            assert!(result.is_err(), "query after close should fail");
        });
    }
}
