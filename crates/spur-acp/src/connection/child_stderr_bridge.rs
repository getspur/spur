//! Per-child stderr bridge: bounded byte-chunk reader → file-rotate writer.
//!
//! Replaces the legacy `Stdio::from(File)` approach where the child held the
//! FD. Now spur owns the writer (so `rm` cannot recreate the deleted-FD
//! pattern) and applies per-child size caps.
//!
//! Key choices, all anchored in the gate-2 review:
//! - **Bounded byte-chunk reads**, not `read_line`. Defends against
//!   `\r`-only progress bars that never deliver a newline.
//! - `non_blocking::lossy(true)` for backpressure: drop on full, do not
//!   block the child's stderr write.
//! - Per-child `buffered_lines_limit` so N=8 children stay ≤ 32 MB in-RAM.

use anyhow::Result;
use file_rotate::{compression::Compression, suffix::AppendCount, ContentLimit, FileRotate};
use std::fs::OpenOptions;
use std::path::Path;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::task::JoinHandle;
use tracing_appender::non_blocking::{ErrorCounter, NonBlocking, NonBlockingBuilder, WorkerGuard};

const READ_BUF_SIZE: usize = 16 * 1024;

pub struct ChildStderrBridge {
    /// Held until shutdown so the worker thread continues draining.
    _guard: WorkerGuard,
    /// Reader task; awaits child stderr EOF.
    reader: JoinHandle<()>,
    /// Counter from the underlying `NonBlocking`. Increments once per
    /// dropped 16 KB chunk under lossy backpressure. We surface it on
    /// shutdown so a chronically lagging child becomes visible in logs.
    dropped_chunks: ErrorCounter,
    /// Agent name for log lines.
    agent_name: String,
    /// Child pid for log lines.
    pid: u32,
}

impl ChildStderrBridge {
    /// Spawn a reader task that pipes the child's stderr into a per-child
    /// `FileRotate` writer behind a `non_blocking` worker.
    pub fn start<R>(
        stderr: R,
        log_dir: &Path,
        agent_name: &str,
        pid: u32,
        max_file_bytes: u64,
        max_files: usize,
        buffered_lines_limit: usize,
    ) -> Result<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let basepath = log_dir.join(format!("{agent_name}-{pid}.log"));

        // file-rotate 0.8 takes an `Option<OpenOptions>` (NOT a raw mode int
        // as 0.7 did). Mirror the pattern from spur-cli/src/log_writer.rs:
        // read+create+append with mode 0o600 on unix.
        let mut open_opts = OpenOptions::new();
        open_opts.read(true).create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open_opts.mode(0o600);
        }

        let rotator = FileRotate::new(
            basepath,
            AppendCount::new(max_files),
            ContentLimit::Bytes(max_file_bytes as usize),
            Compression::OnRotate(0),
            Some(open_opts),
        );
        let (writer, guard) = NonBlockingBuilder::default()
            .lossy(true)
            .buffered_lines_limit(buffered_lines_limit)
            .finish(rotator);

        let dropped_chunks = writer.error_counter();
        let agent = agent_name.to_string();

        let reader = tokio::spawn(reader_loop(stderr, writer, agent.clone(), pid));

        Ok(Self {
            _guard: guard,
            reader,
            dropped_chunks,
            agent_name: agent,
            pid,
        })
    }

    /// Block until reader EOFs (child stderr closed) and emit a single
    /// `child_stderr_lagging` summary if any chunks were dropped under
    /// lossy backpressure.
    pub async fn shutdown(self) {
        let _ = self.reader.await;
        let dropped = self.dropped_chunks.dropped_lines();
        if dropped > 0 {
            tracing::error!(
                agent = %self.agent_name,
                pid = self.pid,
                dropped_chunks = dropped,
                "child_stderr_lagging: bridge dropped chunks due to backpressure"
            );
        }
        // _guard drops here, draining the worker.
    }
}

async fn reader_loop<R>(mut stderr: R, mut writer: NonBlocking, agent: String, pid: u32)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    use std::io::Write;
    let mut buf = [0u8; READ_BUF_SIZE];
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                // `NonBlocking::write_all` under `lossy(true)` always returns
                // `Ok(())`: a full channel silently drops the chunk and bumps
                // the writer's internal `error_counter`. We surface the count
                // on shutdown rather than per-write, so this loop stays a
                // straight pass-through. See `shutdown()`.
                let _ = writer.write_all(&buf[..n]);
            }
            Err(e) => {
                tracing::warn!(
                    agent = %agent, pid = pid, error = %e,
                    "child_stderr_bridge: read error; ending bridge"
                );
                break;
            }
        }
    }
}
