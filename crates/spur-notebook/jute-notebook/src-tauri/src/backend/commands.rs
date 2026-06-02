//! High-level APIs for doing operations over [`KernelConnection`] objects.

use serde::Serialize;
use ts_rs::TS;

use super::{
    wire_protocol::{
        ClearOutput, DisplayData, ErrorReply, ExecuteReply, ExecuteRequest, ExecuteResult,
        InterruptReply, InterruptRequest, KernelInfoReply, KernelInfoRequest, KernelMessage,
        KernelMessageType, KernelStatus, Reply, Status, Stream,
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
#[derive(Debug, Clone, Serialize, TS)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
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
            let msg = iopub_rx.recv().await.map_err(|_| Error::KernelDisconnect)?;
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
}
