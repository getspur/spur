//! High-level APIs for doing operations over [`KernelConnection`] objects.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    wire_protocol::{
        ClearOutput, CommMessage, CommOpen, DisplayData, ErrorReply, ExecuteReply, ExecuteRequest,
        ExecuteResult, InterruptReply, InterruptRequest, KernelInfoReply, KernelInfoRequest,
        KernelMessage, KernelMessageType, KernelStatus, Reply, Status, Stream,
    },
    KernelConnection,
};
use crate::Error;

/// Get information through the KernelInfo command.
pub async fn kernel_info(conn: &KernelConnection) -> Result<KernelInfoReply, Error> {
    let mut req = conn
        .call_shell(KernelMessage::new(
            KernelMessageType::KernelInfoRequest,
            KernelInfoRequest {},
        ))
        .await?;
    let msg = req.get_reply::<KernelInfoReply>().await?;
    match msg.content {
        Reply::Ok(info) => Ok(info),
        Reply::Error(_) | Reply::Abort => Err(Error::KernelDisconnect),
    }
}

/// Events that can be received while running a cell.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case", tag = "event", content = "data")]
pub enum RunCellEvent {
    /// Cell execution was submitted to the kernel.
    Started,

    /// Coarse progress while a compiled-language cell transitions to execution.
    CompileProgress {
        /// Current compile/run phase.
        phase: CompilePhase,
        /// Current compile target, when known.
        current: Option<String>,
    },

    /// Standard output from the kernel.
    Stdout(String),

    /// Standard error from the kernel.
    Stderr(String),

    /// Result of cell execution (i.e., if the last line is an expression).
    ExecuteResult(ExecuteResult),

    /// Display data in a MIME type (e.g., a matplotlib chart).
    DisplayData(DisplayData),

    /// Update previously-displayed data with a display ID.
    UpdateDisplayData(DisplayData),

    /// Clear the output of a cell.
    ClearOutput(ClearOutput),

    /// Open a comm to the frontend for interactive widgets.
    CommOpen(CommOpen),

    /// Send a one-way comm message to the frontend.
    CommMsg(CommMessage),

    /// Close a frontend comm.
    CommClose(CommMessage),

    /// Error if the cell raised an exception.
    Error(ErrorReply),

    /// Special message indicating the kernel disconnected.
    Disconnect(String),

    /// Cell execution reached a terminal shell reply.
    Finished {
        /// Kernel execution count when available.
        exec_count: Option<u32>,
        /// Terminal execution status.
        status: String,
    },
}

/// Coarse compile/run phase for compiled notebook cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CompilePhase {
    /// The kernel has accepted work and compilation may be in progress.
    Compiling,
    /// The first user-visible output indicates code is running.
    Running,
}

/// Compile progress tracking strategy for a running cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompileProgressMode {
    /// Suppress compile progress events.
    None,
    /// Emit coarse compile progress for Cargo-backed cells.
    Cargo,
    /// Emit coarse compile progress for Go build-backed cells.
    GoBuild,
}

/// Compile progress payload emitted by the phase tracker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileProgress {
    /// Current compile/run phase.
    pub phase: CompilePhase,
    /// Current compile target, when known.
    pub current: Option<String>,
}

/// Pure state machine for coarse compile progress emissions.
#[derive(Debug, Clone)]
pub struct CompilePhaseTracker {
    mode: CompileProgressMode,
    compiling_emitted: bool,
    running_emitted: bool,
}

impl CompilePhaseTracker {
    /// Create a tracker for the selected progress mode.
    pub fn new(mode: CompileProgressMode) -> Self {
        Self {
            mode,
            compiling_emitted: false,
            running_emitted: false,
        }
    }

    /// Observe the kernel becoming busy.
    pub fn on_busy(&mut self) -> Option<CompileProgress> {
        self.emit_once(CompilePhase::Compiling)
    }

    /// Observe the first user-visible execution output.
    pub fn on_output(&mut self) -> Option<CompileProgress> {
        self.emit_once(CompilePhase::Running)
    }

    /// Observe a single cargo compile unit (one `Compiling <crate>` line).
    ///
    /// Emits a `Compiling` progress update carrying the crate name while the
    /// cell is still compiling. Returns `None` once execution output has begun
    /// (running phase) or for the [`CompileProgressMode::None`] mode. This does
    /// not touch the [`Self::on_busy`] once-guard, so coarse and per-crate
    /// progress remain independent.
    pub fn on_compile_unit(&mut self, krate: String) -> Option<CompileProgress> {
        if self.mode == CompileProgressMode::None || self.running_emitted {
            return None;
        }
        Some(CompileProgress {
            phase: CompilePhase::Compiling,
            current: Some(krate),
        })
    }

    fn emit_once(&mut self, phase: CompilePhase) -> Option<CompileProgress> {
        if self.mode == CompileProgressMode::None {
            return None;
        }

        let emitted = match phase {
            CompilePhase::Compiling => &mut self.compiling_emitted,
            CompilePhase::Running => &mut self.running_emitted,
        };
        if *emitted {
            return None;
        }

        *emitted = true;
        Some(CompileProgress {
            phase,
            current: None,
        })
    }
}

/// Parse a cargo `   Compiling <crate> v<ver>` progress line.
///
/// Returns the crate name when `line` (after any leading whitespace) is a cargo
/// "Compiling" status line, and `None` for any other output. This is a pure
/// function so it can be unit-tested without a kernel.
pub fn parse_cargo_progress(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix("Compiling ")?;
    let mut parts = rest.split_whitespace();
    let krate = parts.next()?;
    // Cargo follows the crate name with a `v<version>` token; require it so we
    // don't match unrelated lines that merely start with "Compiling ".
    let version = parts.next()?;
    if !version.starts_with('v') {
        return None;
    }
    Some(krate.to_string())
}

async fn send_compile_progress(
    tx: &async_channel::Sender<RunCellEvent>,
    progress: Option<CompileProgress>,
) {
    if let Some(CompileProgress { phase, current }) = progress {
        _ = tx
            .send(RunCellEvent::CompileProgress { phase, current })
            .await;
    }
}

/// Run a code cell, returning the events received in the meantime.
pub async fn run_cell(
    conn: &KernelConnection,
    code: &str,
) -> Result<async_channel::Receiver<RunCellEvent>, Error> {
    run_cell_with_mode(conn, code, CompileProgressMode::None).await
}

/// Run a code cell with compile progress tracking mode.
pub async fn run_cell_with_mode(
    conn: &KernelConnection,
    code: &str,
    mode: CompileProgressMode,
) -> Result<async_channel::Receiver<RunCellEvent>, Error> {
    let mut iopub_rx = conn.subscribe_iopub();
    // evcxr writes `Compiling <crate>` progress to its child stderr rather than
    // over the wire protocol. Subscribe only for Cargo-backed cells; gonb's Go
    // build emits nothing on success, so there is nothing to forward there.
    let mut process_stderr_rx = if mode == CompileProgressMode::Cargo {
        Some(conn.subscribe_process_stderr())
    } else {
        None
    };
    let request = KernelMessage::new(
        KernelMessageType::ExecuteRequest,
        ExecuteRequest {
            code: code.into(),
            silent: false,
            store_history: true,
            user_expressions: Default::default(),
            allow_stdin: false,
            stop_on_error: true,
        },
    );
    let request_id = request.header.msg_id.clone();
    let mut req = conn.call_shell(request).await?;

    let (tx, rx) = async_channel::unbounded();
    let mut compile_tracker = CompilePhaseTracker::new(mode);
    _ = tx.send(RunCellEvent::Started).await;
    send_compile_progress(&tx, compile_tracker.on_busy()).await;

    let tx2 = tx.clone();
    let stream_results_fut = async move {
        let mut status = KernelStatus::Busy;

        while status != KernelStatus::Idle {
            let msg = tokio::select! {
                // Forward cargo crate updates as they arrive on the child's
                // stderr. A broadcast `Lagged`/`Closed` must never error the
                // run, so we skip on either and stop polling once closed.
                line = async {
                    match process_stderr_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                }, if process_stderr_rx.is_some() => {
                    match line {
                        Ok(line) => {
                            if let Some(krate) = parse_cargo_progress(&line) {
                                send_compile_progress(
                                    &tx,
                                    compile_tracker.on_compile_unit(krate),
                                )
                                .await;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            process_stderr_rx = None;
                        }
                    }
                    continue;
                }
                msg = iopub_rx.recv() => {
                    msg.map_err(|_| Error::KernelDisconnect)?
                }
            };
            if msg
                .parent_header
                .as_ref()
                .map(|header| header.msg_id.as_str())
                != Some(request_id.as_str())
            {
                continue;
            }
            match msg.header.msg_type {
                KernelMessageType::Status => {
                    let msg = msg.into_typed::<Status>()?;
                    status = msg.content.execution_state;
                }
                KernelMessageType::Stream => {
                    let msg = msg.into_typed::<Stream>()?;
                    if msg.content.name == "stdout" {
                        send_compile_progress(&tx, compile_tracker.on_output()).await;
                        _ = tx.send(RunCellEvent::Stdout(msg.content.text)).await;
                    } else {
                        _ = tx.send(RunCellEvent::Stderr(msg.content.text)).await;
                    }
                }
                // We ignore ExecuteInput messages since they just echo the input code.
                KernelMessageType::ExecuteInput => {}
                KernelMessageType::ExecuteResult => {
                    let msg = msg.into_typed::<ExecuteResult>()?;
                    send_compile_progress(&tx, compile_tracker.on_output()).await;
                    _ = tx.send(RunCellEvent::ExecuteResult(msg.content)).await;
                }
                KernelMessageType::DisplayData => {
                    let msg = msg.into_typed::<DisplayData>()?;
                    send_compile_progress(&tx, compile_tracker.on_output()).await;
                    _ = tx.send(RunCellEvent::DisplayData(msg.content)).await;
                }
                KernelMessageType::UpdateDisplayData => {
                    let msg = msg.into_typed::<DisplayData>()?;
                    _ = tx.send(RunCellEvent::UpdateDisplayData(msg.content)).await;
                }
                KernelMessageType::ClearOutput => {
                    let msg = msg.into_typed::<ClearOutput>()?;
                    _ = tx.send(RunCellEvent::ClearOutput(msg.content)).await;
                }
                KernelMessageType::Error => {
                    let msg = msg.into_typed::<ErrorReply>()?;
                    _ = tx.send(RunCellEvent::Error(msg.content)).await;
                }
                _ => {}
            }
        }

        let reply = req.get_reply::<ExecuteReply>().await?;
        let (exec_count, status) = match reply.content {
            Reply::Ok(reply) => (u32::try_from(reply.execution_count).ok(), "ok".to_string()),
            Reply::Error(_) => (None, "error".to_string()),
            Reply::Abort => (None, "abort".to_string()),
        };
        _ = tx.send(RunCellEvent::Finished { exec_count, status }).await;

        Ok::<_, Error>(())
    };

    tokio::spawn(async move {
        // Translate any errors into a disconnect message.
        if let Err(err) = stream_results_fut.await {
            _ = tx2.send(RunCellEvent::Disconnect(err.to_string())).await;
        }
    });

    Ok(rx)
}

/// Interrupt the kernel's current operation.
pub async fn interrupt(conn: &KernelConnection) -> Result<(), Error> {
    let mut req = conn
        .call_control(KernelMessage::new(
            KernelMessageType::InterruptRequest,
            InterruptRequest {},
        ))
        .await?;
    match req.get_reply::<InterruptReply>().await?.content {
        Reply::Ok(_) => Ok(()),
        Reply::Error(_) | Reply::Abort => Err(Error::KernelDisconnect),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_phase_tracker_emits_compiling_once_for_compile_modes() {
        for mode in [CompileProgressMode::Cargo, CompileProgressMode::GoBuild] {
            let mut tracker = CompilePhaseTracker::new(mode);

            let progress = tracker.on_busy().expect("compile mode emits compiling");
            assert_eq!(progress.phase, CompilePhase::Compiling);
            assert_eq!(progress.current, None);
            assert!(tracker.on_busy().is_none());
        }

        let mut tracker = CompilePhaseTracker::new(CompileProgressMode::None);
        assert!(tracker.on_busy().is_none());
    }

    #[test]
    fn compile_phase_tracker_emits_running_once_for_compile_modes() {
        for mode in [CompileProgressMode::Cargo, CompileProgressMode::GoBuild] {
            let mut tracker = CompilePhaseTracker::new(mode);
            _ = tracker.on_busy();

            let progress = tracker.on_output().expect("compile mode emits running");
            assert_eq!(progress.phase, CompilePhase::Running);
            assert_eq!(progress.current, None);
            assert!(tracker.on_output().is_none());
        }

        let mut tracker = CompilePhaseTracker::new(CompileProgressMode::None);
        assert!(tracker.on_output().is_none());
    }

    #[test]
    fn parse_cargo_progress_extracts_crate_name() {
        assert_eq!(
            parse_cargo_progress("   Compiling smawk v0.3.2"),
            Some("smawk".to_string())
        );
        assert_eq!(
            parse_cargo_progress("   Compiling textwrap v0.16.2"),
            Some("textwrap".to_string())
        );
    }

    #[test]
    fn parse_cargo_progress_ignores_non_compiling_lines() {
        assert_eq!(parse_cargo_progress("hello from a cell"), None);
        assert_eq!(parse_cargo_progress(""), None);
        assert_eq!(parse_cargo_progress("Compiling"), None);
        assert_eq!(parse_cargo_progress("   Finished dev [unoptimized]"), None);
    }

    #[test]
    fn on_compile_unit_emits_crate_while_compiling() {
        let mut tracker = CompilePhaseTracker::new(CompileProgressMode::Cargo);
        _ = tracker.on_busy();

        let progress = tracker
            .on_compile_unit("smawk".to_string())
            .expect("cargo mode emits crate updates while compiling");
        assert_eq!(progress.phase, CompilePhase::Compiling);
        assert_eq!(progress.current, Some("smawk".to_string()));

        // Crate updates keep flowing and do not consume the on_busy once-guard.
        let next = tracker
            .on_compile_unit("textwrap".to_string())
            .expect("cargo mode keeps emitting crate updates");
        assert_eq!(next.current, Some("textwrap".to_string()));
    }

    #[test]
    fn on_compile_unit_stops_after_output_begins() {
        let mut tracker = CompilePhaseTracker::new(CompileProgressMode::Cargo);
        _ = tracker.on_busy();
        _ = tracker.on_output();

        assert!(tracker.on_compile_unit("smawk".to_string()).is_none());
    }

    #[test]
    fn on_compile_unit_suppressed_for_none_mode() {
        let mut tracker = CompilePhaseTracker::new(CompileProgressMode::None);
        assert!(tracker.on_compile_unit("smawk".to_string()).is_none());
    }
}
