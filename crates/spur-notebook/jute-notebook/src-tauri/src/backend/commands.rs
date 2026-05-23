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

/// Run a code cell, returning the events received in the meantime.
pub async fn run_cell(
    conn: &KernelConnection,
    code: &str,
) -> Result<async_channel::Receiver<RunCellEvent>, Error> {
    let mut iopub_rx = conn.subscribe_iopub();
    let mut req = conn
        .call_shell(KernelMessage::new(
            KernelMessageType::ExecuteRequest,
            ExecuteRequest {
                code: code.into(),
                silent: false,
                store_history: true,
                user_expressions: Default::default(),
                allow_stdin: false,
                stop_on_error: true,
            },
        ))
        .await?;

    let (tx, rx) = async_channel::unbounded();
    _ = tx.send(RunCellEvent::Started).await;

    let tx2 = tx.clone();
    let stream_results_fut = async move {
        let mut status = KernelStatus::Busy;

        while status != KernelStatus::Idle {
            let msg = iopub_rx.recv().await.map_err(|_| Error::KernelDisconnect)?;
            match msg.header.msg_type {
                KernelMessageType::Status => {
                    let msg = msg.into_typed::<Status>()?;
                    status = msg.content.execution_state;
                }
                KernelMessageType::Stream => {
                    let msg = msg.into_typed::<Stream>()?;
                    if msg.content.name == "stdout" {
                        _ = tx.send(RunCellEvent::Stdout(msg.content.text)).await;
                    } else {
                        _ = tx.send(RunCellEvent::Stderr(msg.content.text)).await;
                    }
                }
                // We ignore ExecuteInput messages since they just echo the input code.
                KernelMessageType::ExecuteInput => {}
                KernelMessageType::ExecuteResult => {
                    let msg = msg.into_typed::<ExecuteResult>()?;
                    _ = tx.send(RunCellEvent::ExecuteResult(msg.content)).await;
                }
                KernelMessageType::DisplayData => {
                    let msg = msg.into_typed::<DisplayData>()?;
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
