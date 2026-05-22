//! Invoke handlers for commands callable from the frontend.

use std::{
    env, fs,
    future::Future,
    io::{self, Write},
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use dashmap::mapref::entry::Entry;
use serde::Serialize;
use sysinfo::{Pid, System};
use tauri::{ipc::Channel, WebviewWindow};
use tokio::sync::Mutex;
use tracing::info;
use uuid::Uuid;

use crate::{
    backend::{
        commands::{self, RunCellEvent},
        local::{environment, KernelUsageInfo, LocalKernel},
        notebook::NotebookRoot,
    },
    state::{notebook_slot_id, window_slot_id, KernelSlot, State},
    Error,
};

pub mod venv;

type SaveFuture = Pin<Box<dyn Future<Output = Result<(), Error>> + Send>>;
type SaveWriter = dyn Fn(PathBuf, NotebookRoot) -> SaveFuture + Send + Sync;

/// Snapshot of a stable kernel slot for agent read-side tools.
#[derive(Debug, Clone, Serialize)]
pub struct KernelSlotInfo {
    /// Stable slot ID used by the frontend and Tauri command layer.
    pub kernel_id: String,
    /// Kernel spec name used for the latest successful start.
    pub spec_name: String,
    /// Monotonic in-memory generation for this kernel slot.
    pub generation: u64,
    /// Coarse status for the slot.
    pub status: String,
    /// Kernel CPU usage normalized across available CPUs.
    pub cpu_pct: f32,
    /// Kernel resident memory in MiB.
    pub mem_mb: f32,
}

/// Coordinates disk saves so only one notebook write runs at a time.
#[derive(Clone)]
pub(crate) struct SaveCoordinator {
    inner: Arc<Mutex<SaveState>>,
    writer: Arc<SaveWriter>,
}

#[derive(Default)]
struct SaveState {
    in_flight: bool,
    queued: Option<PendingSave>,
}

struct PendingSave {
    path: PathBuf,
    contents: NotebookRoot,
}

impl Default for SaveCoordinator {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SaveState::default())),
            writer: Arc::new(|path, contents| {
                Box::pin(async move { atomic_write_notebook(&path, &contents).await })
            }),
        }
    }
}

impl SaveCoordinator {
    async fn save(&self, path: PathBuf, contents: NotebookRoot) -> Result<(), Error> {
        let mut state = self.inner.lock().await;
        state.queued = Some(PendingSave { path, contents });
        if state.in_flight {
            return Ok(());
        }
        state.in_flight = true;
        drop(state);

        loop {
            let pending = {
                let mut state = self.inner.lock().await;
                state.queued.take().expect("save queue must contain work")
            };

            let result = (self.writer)(pending.path, pending.contents).await;
            let mut state = self.inner.lock().await;
            if let Err(error) = result {
                state.in_flight = false;
                return Err(error);
            }

            if state.queued.is_none() {
                state.in_flight = false;
                return Ok(());
            }
        }
    }

    #[cfg(test)]
    fn with_writer_for_test<F>(writer: F) -> Self
    where
        F: Fn(PathBuf, NotebookRoot) -> SaveFuture + Send + Sync + 'static,
    {
        Self {
            inner: Arc::new(Mutex::new(SaveState::default())),
            writer: Arc::new(writer),
        }
    }
}

async fn atomic_write_notebook(path: &Path, contents: &NotebookRoot) -> Result<(), Error> {
    let path = path.to_owned();
    let contents = contents.clone();
    tokio::task::spawn_blocking(move || atomic_write_notebook_blocking(&path, &contents))
        .await
        .map_err(|error| Error::Filesystem(io::Error::other(error)))?
}

fn atomic_write_notebook_blocking(path: &Path, contents: &NotebookRoot) -> Result<(), Error> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            Error::Filesystem(io::Error::new(
                io::ErrorKind::InvalidInput,
                "notebook path must have a file name",
            ))
        })?;
    let temp_path = path.with_file_name(format!(".{file_name}.{}.tmp", Uuid::new_v4()));

    let write_result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .map_err(Error::Filesystem)?;
        serde_json::to_writer_pretty(&mut file, contents)?;
        writeln!(&mut file).map_err(Error::Filesystem)?;
        file.sync_all().map_err(Error::Filesystem)?;
        Ok(())
    })();

    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    fs::rename(&temp_path, path).map_err(Error::Filesystem)
}

fn slot_id_for_window(window: &WebviewWindow) -> String {
    notebook_path_from_window(window)
        .map(|path| notebook_slot_id(&path))
        .unwrap_or_else(|| window_slot_id(window.label()))
}

fn notebook_path_from_window(window: &WebviewWindow) -> Option<String> {
    let url = window.url().ok()?;
    url.query_pairs()
        .find_map(|(key, value)| (key == "path").then(|| value.into_owned()))
        .filter(|path| !path.is_empty())
}

async fn start_local_kernel(spec_name: &str) -> Result<LocalKernel, Error> {
    // Temporary hack to just start a kernel locally with ZeroMQ.
    let kernels = environment::list_kernels(None).await;
    let mut kernel_spec = match kernels
        .iter()
        .find(|(path, _spec)| path.file_name().and_then(|s| s.to_str()) == Some(spec_name))
    {
        Some((_, kernel_spec)) => kernel_spec.clone(),
        None => {
            return Err(Error::KernelConnect(format!(
                "no kernel named {spec_name:?} found"
            )))
        }
    };

    if let Some(command) = kernel_spec.argv.first_mut() {
        if command == "python" {
            if let Ok(python_path) = env::var("PYTHON_PATH") {
                *command = python_path;
            } else {
                // Temporary hack
                *command = "/opt/homebrew/bin/python3.11".into();
            }
        }
    }

    let kernel = LocalKernel::start(&kernel_spec).await?;

    let info = commands::kernel_info(kernel.conn()).await?;
    info!(banner = info.banner, "started new jute kernel");

    Ok(kernel)
}

fn install_kernel_in_slot(
    state: &State,
    slot_id: &str,
    spec_name: String,
    kernel: LocalKernel,
) -> (u64, Option<LocalKernel>) {
    match state.kernels.entry(slot_id.to_string()) {
        Entry::Occupied(mut entry) => {
            let slot = entry.get_mut();
            let previous_kernel = slot.kernel.take();
            let generation = slot.replace_kernel(kernel, spec_name);
            (generation, previous_kernel)
        }
        Entry::Vacant(entry) => {
            let slot = KernelSlot::with_kernel(kernel, spec_name);
            let generation = slot.generation();
            entry.insert(slot);
            (generation, None)
        }
    }
}

fn take_kernel_if_present(state: &State, slot_id: &str) -> Option<LocalKernel> {
    let mut slot = state.kernels.get_mut(slot_id)?;
    slot.kernel.take()
}

fn take_kernel_from_slot(state: &State, slot_id: &str) -> Result<LocalKernel, Error> {
    let mut slot = state
        .kernels
        .get_mut(slot_id)
        .ok_or(Error::KernelDisconnect)?;
    slot.kernel.take().ok_or(Error::KernelDisconnect)
}

fn restore_kernel_to_slot(state: &State, slot_id: &str, kernel: LocalKernel) {
    if let Some(mut slot) = state.kernels.get_mut(slot_id) {
        slot.kernel = Some(kernel);
    }
}

fn kernel_connection_for_slot(
    state: &State,
    slot_id: &str,
) -> Result<crate::backend::KernelConnection, Error> {
    let slot = state.kernels.get(slot_id).ok_or(Error::KernelDisconnect)?;
    Ok(slot
        .kernel
        .as_ref()
        .ok_or(Error::KernelDisconnect)?
        .conn()
        .clone())
}

fn spec_name_for_slot(state: &State, slot_id: &str) -> Result<String, Error> {
    let slot = state.kernels.get(slot_id).ok_or(Error::KernelDisconnect)?;
    Ok(slot.spec_name().to_string())
}

struct KernelProcessUsage {
    cpu_consumed: f32,
    cpu_available: f32,
    memory_consumed: f32,
    memory_available: f32,
}

async fn kernel_process_usage(pid: Pid) -> Option<KernelProcessUsage> {
    let mut system = System::new_all();
    system.refresh_all();
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    system.refresh_process(pid);

    system.process(pid).map(|process| KernelProcessUsage {
        cpu_consumed: process.cpu_usage(),
        cpu_available: system.cpus().len() as f32,
        memory_consumed: process.memory() as f32,
        memory_available: system.total_memory() as f32,
    })
}

/// Measure the kernel's CPU and memory usage as a percentage of total system
/// resources.
#[tauri::command]
pub async fn kernel_usage_info(
    kernel_id: &str,
    state: tauri::State<'_, State>,
) -> Result<KernelUsageInfo, Error> {
    let pid: Pid = {
        let slot = state.kernels.get(kernel_id).ok_or(Error::KernelNotFound)?;
        let kernel = slot.kernel.as_ref().ok_or(Error::KernelNotFound)?;
        Pid::from_u32(kernel.pid().ok_or(Error::KernelProcessNotFound)?)
    };

    if let Some(usage) = kernel_process_usage(pid).await {
        Ok(KernelUsageInfo {
            cpu_consumed: usage.cpu_consumed,
            cpu_available: usage.cpu_available,
            memory_consumed: usage.memory_consumed,
            memory_available: usage.memory_available,
        })
    } else {
        Err(Error::KernelProcessNotFound)
    }
}

/// Return read-side metadata for a stable kernel slot.
#[tauri::command]
pub async fn kernel_slot_info(
    kernel_id: &str,
    state: tauri::State<'_, State>,
) -> Result<KernelSlotInfo, Error> {
    let (spec_name, generation, pid) = {
        let slot = state.kernels.get(kernel_id).ok_or(Error::KernelNotFound)?;
        (
            slot.spec_name().to_string(),
            slot.generation(),
            slot.kernel.as_ref().and_then(|kernel| kernel.pid()),
        )
    };

    let (status, cpu_pct, mem_mb) = match pid {
        Some(pid) => match kernel_process_usage(Pid::from_u32(pid)).await {
            Some(usage) => {
                let cpu_pct = if usage.cpu_available > 0.0 {
                    (usage.cpu_consumed / usage.cpu_available) * 100.0
                } else {
                    0.0
                };
                (
                    "idle".to_string(),
                    cpu_pct,
                    usage.memory_consumed / 1024.0 / 1024.0,
                )
            }
            None => ("dead".to_string(), 0.0, 0.0),
        },
        None => ("dead".to_string(), 0.0, 0.0),
    };

    Ok(KernelSlotInfo {
        kernel_id: kernel_id.to_string(),
        spec_name,
        generation,
        status,
        cpu_pct,
        mem_mb,
    })
}

/// Start a new Jupyter kernel.
#[tauri::command]
pub async fn start_kernel(
    spec_name: &str,
    window: WebviewWindow,
    state: tauri::State<'_, State>,
) -> Result<String, Error> {
    // TODO: Save the client in a better place.
    // let client = JupyterClient::new("", "")?;

    let slot_id = slot_id_for_window(&window);
    if let Some(mut kernel) = take_kernel_if_present(&state, &slot_id) {
        if let Err(error) = kernel.kill().await {
            restore_kernel_to_slot(&state, &slot_id, kernel);
            return Err(error);
        }
    }

    let kernel = start_local_kernel(spec_name).await?;
    let (generation, _previous_kernel) =
        install_kernel_in_slot(&state, &slot_id, spec_name.to_string(), kernel);
    info!(slot_id = %slot_id, generation, "started jute kernel slot");

    Ok(slot_id)
}

/// Restart a Jupyter kernel in an existing stable slot.
#[tauri::command]
pub async fn restart_kernel(
    slot_id: &str,
    spec_name: Option<String>,
    state: tauri::State<'_, State>,
) -> Result<String, Error> {
    info!("restarting jute kernel slot {slot_id}");
    let next_spec_name = match spec_name {
        Some(spec_name) => spec_name,
        None => spec_name_for_slot(&state, slot_id)?,
    };

    let mut kernel = take_kernel_from_slot(&state, slot_id)?;
    if let Err(error) = kernel.kill().await {
        restore_kernel_to_slot(&state, slot_id, kernel);
        return Err(error);
    }

    let kernel = start_local_kernel(&next_spec_name).await?;
    let (generation, _previous_kernel) =
        install_kernel_in_slot(&state, slot_id, next_spec_name, kernel);
    info!(slot_id = %slot_id, generation, "restarted jute kernel slot");

    Ok(slot_id.to_string())
}

/// Stop a Jupyter kernel.
#[tauri::command]
pub async fn stop_kernel(kernel_id: &str, state: tauri::State<'_, State>) -> Result<(), Error> {
    info!("stopping jute kernel slot {kernel_id}");
    let mut kernel = take_kernel_from_slot(&state, kernel_id)?;
    if let Err(error) = kernel.kill().await {
        restore_kernel_to_slot(&state, kernel_id, kernel);
        return Err(error);
    }
    Ok(())
}

/// Get the contents of a Jupyter notebook on disk.
#[tauri::command]
pub async fn get_notebook(path: &str) -> Result<NotebookRoot, Error> {
    info!("getting notebook at {path}");

    let contents = tokio::fs::read_to_string(path)
        .await
        .map_err(Error::Filesystem)?;
    Ok(serde_json::from_str(&contents)?)
}

/// Save the contents of a Jupyter notebook to disk.
#[tauri::command]
pub async fn save_to_disk(
    path: &str,
    contents: NotebookRoot,
    state: tauri::State<'_, State>,
) -> Result<(), Error> {
    info!("saving notebook at {path}");
    state
        .save_coordinator
        .save(PathBuf::from(path), contents)
        .await
}

/// Run a code cell in a Jupyter kernel.
pub async fn run_cell_events(
    kernel_id: &str,
    code: &str,
    state: &State,
) -> Result<async_channel::Receiver<RunCellEvent>, Error> {
    let conn = kernel_connection_for_slot(state, kernel_id)?;
    commands::run_cell(&conn, code).await
}

/// Interrupt a Jupyter kernel slot.
pub async fn interrupt_kernel_slot(kernel_id: &str, state: &State) -> Result<(), Error> {
    let conn = kernel_connection_for_slot(state, kernel_id)?;
    commands::interrupt(&conn).await
}

/// Run a code cell in a Jupyter kernel.
#[tauri::command]
pub async fn run_cell(
    kernel_id: &str,
    code: &str,
    on_event: Channel<RunCellEvent>,
    state: tauri::State<'_, State>,
) -> Result<(), Error> {
    let rx = run_cell_events(kernel_id, code, &state).await?;
    while let Ok(event) = rx.recv().await {
        if on_event.send(event).is_err() {
            break;
        }
    }
    Ok(())
}

/// Interrupt a running Jupyter kernel.
#[tauri::command]
pub async fn interrupt_kernel(
    kernel_id: &str,
    state: tauri::State<'_, State>,
) -> Result<(), Error> {
    interrupt_kernel_slot(kernel_id, &state).await
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    use tokio::sync::{oneshot, Mutex};

    use super::*;
    use crate::backend::notebook::{
        Cell, CellMetadata, CodeCell, MultilineString, NotebookMetadata, SpurCellMetadata,
    };

    fn notebook_with_source(source: &str, version: u64) -> NotebookRoot {
        NotebookRoot {
            metadata: NotebookMetadata {
                kernelspec: None,
                language_info: None,
                orig_nbformat: None,
                title: None,
                authors: None,
                other: Default::default(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells: vec![Cell::Code(CodeCell {
                id: Some("550e8400-e29b-41d4-a716-446655440000".to_string()),
                metadata: CellMetadata {
                    spur: Some(SpurCellMetadata { version }),
                    other: Default::default(),
                },
                source: MultilineString::Single(source.to_string()),
                execution_count: None,
                outputs: Vec::new(),
            })],
        }
    }

    fn first_source(contents: &NotebookRoot) -> String {
        let Cell::Code(cell) = &contents.cells[0] else {
            panic!("expected code cell");
        };
        match &cell.source {
            MultilineString::Single(source) => source.clone(),
            MultilineString::Multi(lines) => lines.join(""),
        }
    }

    #[tokio::test]
    async fn save_coordinator_collapses_queued_saves_to_latest() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let call_count = Arc::new(AtomicUsize::new(0));
        let (first_started_tx, first_started_rx) = oneshot::channel();
        let (allow_first_tx, allow_first_rx) = oneshot::channel();
        let first_started_tx = Arc::new(Mutex::new(Some(first_started_tx)));
        let allow_first_rx = Arc::new(Mutex::new(Some(allow_first_rx)));

        let coordinator = SaveCoordinator::with_writer_for_test({
            let writes = Arc::clone(&writes);
            let call_count = Arc::clone(&call_count);
            let first_started_tx = Arc::clone(&first_started_tx);
            let allow_first_rx = Arc::clone(&allow_first_rx);
            move |_path: PathBuf, contents: NotebookRoot| {
                let writes = Arc::clone(&writes);
                let call_count = Arc::clone(&call_count);
                let first_started_tx = Arc::clone(&first_started_tx);
                let allow_first_rx = Arc::clone(&allow_first_rx);
                Box::pin(async move {
                    let index = call_count.fetch_add(1, Ordering::SeqCst);
                    if index == 0 {
                        if let Some(sender) = first_started_tx.lock().await.take() {
                            let _ = sender.send(());
                        }
                        if let Some(receiver) = allow_first_rx.lock().await.take() {
                            let _ = receiver.await;
                        }
                    }

                    writes.lock().await.push(first_source(&contents));
                    Ok(())
                })
            }
        });

        let path = PathBuf::from("notebook.ipynb");
        let first = {
            let coordinator = coordinator.clone();
            let path = path.clone();
            tokio::spawn(async move {
                coordinator
                    .save(path, notebook_with_source("first", 1))
                    .await
            })
        };

        first_started_rx.await.unwrap();
        coordinator
            .save(path.clone(), notebook_with_source("second", 2))
            .await
            .unwrap();
        coordinator
            .save(path, notebook_with_source("third", 3))
            .await
            .unwrap();
        allow_first_tx.send(()).unwrap();
        first.await.unwrap().unwrap();

        assert_eq!(
            writes.lock().await.as_slice(),
            ["first".to_string(), "third".to_string()]
        );
    }

    #[tokio::test]
    async fn atomic_write_notebook_writes_parseable_json() {
        let dir = std::env::temp_dir().join(format!("jute-save-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notebook.ipynb");

        atomic_write_notebook(&path, &notebook_with_source("persisted", 9))
            .await
            .unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: NotebookRoot = serde_json::from_str(&contents).unwrap();
        let Cell::Code(cell) = &parsed.cells[0] else {
            panic!("expected code cell");
        };
        assert_eq!(
            cell.id.as_deref(),
            Some("550e8400-e29b-41d4-a716-446655440000")
        );
        assert_eq!(cell.metadata.spur.as_ref().unwrap().version, 9);

        std::fs::remove_dir_all(dir).unwrap();
    }
}
