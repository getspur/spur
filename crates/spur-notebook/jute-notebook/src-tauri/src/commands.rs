//! Invoke handlers for commands callable from the frontend.

use std::{
    env, fs,
    future::Future,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use dashmap::mapref::entry::Entry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sysinfo::{Pid, System};
use tauri::{ipc::Channel, AppHandle, WebviewWindow};
use tokio::sync::Mutex;
use tracing::info;
use ts_rs::TS;
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

/// Recent notebook entry enriched for the Tauri webview.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct RecentNotebookEntry {
    /// Absolute notebook path.
    pub path: String,
    /// Last opened time in RFC3339 format.
    pub last_opened: String,
    /// Whether the notebook lives under the scratch directory.
    pub is_scratch: bool,
    /// Whether the notebook is pinned in recents.
    pub pinned: bool,
    /// Whether the path-derived kernel slot has a live kernel.
    pub kernel_alive: bool,
    /// Whether this entry is the daemon's current notebook.
    pub is_current: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaemonRecentEntry {
    path: PathBuf,
    last_opened: String,
    is_scratch: bool,
    pinned: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DaemonControlResponse {
    ok: bool,
    path: Option<String>,
    entries: Option<Vec<DaemonRecentEntry>>,
    error: Option<DaemonControlError>,
}

#[derive(Debug, Deserialize)]
struct DaemonControlError {
    code: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct LastNotebookRecord {
    path: PathBuf,
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
    atomic_write_notebook_blocking_with_hook(path, contents, |_| Ok(()))
}

fn atomic_write_notebook_blocking_with_hook<F>(
    path: &Path,
    contents: &NotebookRoot,
    before_rename: F,
) -> Result<(), Error>
where
    F: FnOnce(&Path) -> Result<(), Error>,
{
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

    if let Err(error) = before_rename(&temp_path) {
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

fn daemon_socket_path_from_args() -> Result<PathBuf, Error> {
    let mut args = env::args();
    while let Some(arg) = args.next() {
        if arg == "--socket" {
            return args.next().map(PathBuf::from).ok_or_else(|| {
                Error::NotebookDaemon("--socket requires a notebook daemon path".to_string())
            });
        }
    }
    Err(Error::NotebookDaemon(
        "notebook daemon socket path was not provided".to_string(),
    ))
}

#[cfg(unix)]
async fn write_daemon_frame<W>(writer: &mut W, bytes: &[u8]) -> Result<(), Error>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(Error::NotebookDaemon(format!(
            "daemon request frame is too large: {} bytes",
            bytes.len()
        )));
    }
    use tokio::io::AsyncWriteExt;
    writer
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .await
        .map_err(Error::Filesystem)?;
    writer.write_all(bytes).await.map_err(Error::Filesystem)?;
    writer.flush().await.map_err(Error::Filesystem)
}

#[cfg(unix)]
async fn read_daemon_frame<R>(reader: &mut R) -> Result<Vec<u8>, Error>
where
    R: tokio::io::AsyncRead + Unpin,
{
    const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;
    use tokio::io::AsyncReadExt;
    let mut len = [0_u8; 4];
    reader
        .read_exact(&mut len)
        .await
        .map_err(Error::Filesystem)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(Error::NotebookDaemon(format!(
            "daemon response frame is too large: {len} bytes"
        )));
    }
    let mut bytes = vec![0_u8; len];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(Error::Filesystem)?;
    Ok(bytes)
}

#[cfg(unix)]
async fn send_daemon_control(
    command: &str,
    path: Option<&Path>,
    pinned: Option<bool>,
) -> Result<DaemonControlResponse, Error> {
    use tokio::net::UnixStream;

    let socket_path = daemon_socket_path_from_args()?;
    let mut request = json!({
        "daemon": "notebook.v1",
        "command": command,
    });
    if let Some(path) = path {
        request["path"] = Value::String(path.display().to_string());
    }
    if let Some(pinned) = pinned {
        request["pinned"] = Value::Bool(pinned);
    }

    let mut stream = UnixStream::connect(&socket_path)
        .await
        .map_err(Error::Filesystem)?;
    write_daemon_frame(&mut stream, &serde_json::to_vec(&request)?).await?;
    let bytes = read_daemon_frame(&mut stream).await?;
    let response: DaemonControlResponse = serde_json::from_slice(&bytes)?;
    if response.ok {
        Ok(response)
    } else {
        let message = response
            .error
            .map(|error| format!("{}: {}", error.code, error.message))
            .unwrap_or_else(|| "daemon command failed without an error body".to_string());
        Err(Error::NotebookDaemon(message))
    }
}

#[cfg(not(unix))]
async fn send_daemon_control(
    _command: &str,
    _path: Option<&Path>,
    _pinned: Option<bool>,
) -> Result<DaemonControlResponse, Error> {
    Err(Error::NotebookDaemon(
        "notebook daemon socket commands are only available on Unix platforms".to_string(),
    ))
}

fn home_dir() -> Result<PathBuf, Error> {
    if let Some(home) = env::var_os("HOME") {
        return Ok(PathBuf::from(home));
    }
    if let Some(home) = env::var_os("USERPROFILE") {
        return Ok(PathBuf::from(home));
    }
    #[cfg(windows)]
    if let (Some(drive), Some(path)) = (env::var_os("HOMEDRIVE"), env::var_os("HOMEPATH")) {
        let mut home = PathBuf::from(drive);
        home.push(path);
        return Ok(home);
    }
    Err(Error::Filesystem(io::Error::new(
        io::ErrorKind::NotFound,
        "could not resolve home directory",
    )))
}

fn notebooks_dir() -> Result<PathBuf, Error> {
    Ok(home_dir()?.join(".spur").join("notebooks"))
}

fn scratch_dir() -> Result<PathBuf, Error> {
    Ok(home_dir()?.join(".spur").join("scratch"))
}

fn last_notebook_record_path() -> Result<PathBuf, Error> {
    Ok(notebooks_dir()?.join("last.json"))
}

async fn load_current_notebook_path() -> Result<Option<PathBuf>, Error> {
    let record_path = last_notebook_record_path()?;
    let bytes = match tokio::fs::read(&record_path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::Filesystem(error)),
    };
    let record: LastNotebookRecord = serde_json::from_slice(&bytes)?;
    Ok(Some(record.path))
}

async fn load_current_notebook_path_normalized() -> Result<Option<PathBuf>, Error> {
    match load_current_notebook_path().await? {
        Some(path) => normalize_path(&path).await.map(Some),
        None => Ok(None),
    }
}

async fn normalize_path(path: &Path) -> Result<PathBuf, Error> {
    match tokio::fs::canonicalize(path).await {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => lexical_normalize(path),
        Err(error) => Err(Error::Filesystem(error)),
    }
}

fn lexical_normalize(path: &Path) -> Result<PathBuf, Error> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir().map_err(Error::Filesystem)?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !is_root_or_prefix_only(&normalized) {
                    normalized.pop();
                }
            }
            Component::Normal(segment) => normalized.push(segment),
        }
    }
    Ok(normalized)
}

fn is_root_or_prefix_only(path: &Path) -> bool {
    let mut saw_anchor = false;
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => saw_anchor = true,
            _ => return false,
        }
    }
    saw_anchor
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
    kernel_slot_info_for_state(kernel_id, &state).await
}

async fn kernel_slot_info_for_state(
    kernel_id: &str,
    state: &State,
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

async fn kernel_alive_for_notebook(path: &str, state: &State) -> bool {
    let slot_id = notebook_slot_id(path);
    kernel_slot_info_for_state(&slot_id, state)
        .await
        .map(|info| info.status != "dead")
        .unwrap_or(false)
}

/// List daemon recent notebooks with Tauri-side status metadata.
#[tauri::command]
pub async fn list_recent_notebooks(
    state: tauri::State<'_, State>,
) -> Result<Vec<RecentNotebookEntry>, Error> {
    let response = send_daemon_control("list_recents", None, None).await?;
    let current_path = load_current_notebook_path_normalized().await?;
    let entries = response.entries.unwrap_or_default();
    let mut recent_notebooks = Vec::with_capacity(entries.len());
    for entry in entries {
        let normalized_path = normalize_path(&entry.path).await?;
        let path = normalized_path.display().to_string();
        let kernel_alive = kernel_alive_for_notebook(&path, &state).await;
        let is_current = current_path.as_ref() == Some(&normalized_path);
        recent_notebooks.push(RecentNotebookEntry {
            path,
            last_opened: entry.last_opened,
            is_scratch: entry.is_scratch,
            pinned: entry.pinned,
            kernel_alive,
            is_current,
        });
    }
    Ok(recent_notebooks)
}

/// Remove a notebook path from daemon recents.
#[tauri::command]
pub async fn remove_notebook_from_recents(path: String) -> Result<(), Error> {
    send_daemon_control("remove_from_recents", Some(Path::new(&path)), None)
        .await
        .map(|_| ())
}

/// Set a notebook's daemon recents pin state.
#[tauri::command]
pub async fn set_notebook_pinned(path: String, pinned: bool) -> Result<(), Error> {
    send_daemon_control("set_pinned", Some(Path::new(&path)), Some(pinned))
        .await
        .map(|_| ())
}

/// Create a new scratch notebook through the daemon and return its path.
#[tauri::command]
pub async fn new_notebook_via_daemon() -> Result<String, Error> {
    send_daemon_control("new", None, None)
        .await?
        .path
        .ok_or_else(|| Error::NotebookDaemon("daemon new response did not include path".into()))
}

/// Reopen the daemon's current notebook window and return its path.
#[tauri::command]
pub async fn reopen_notebook_via_daemon() -> Result<String, Error> {
    send_daemon_control("reopen", None, None)
        .await?
        .path
        .ok_or_else(|| Error::NotebookDaemon("daemon reopen response did not include path".into()))
}

/// Close the daemon's current notebook window.
#[tauri::command]
pub async fn close_notebook_via_daemon() -> Result<(), Error> {
    send_daemon_control("close", None, None).await.map(|_| ())
}

/// Move a notebook file to the OS trash unless it is currently loaded.
#[tauri::command]
pub async fn move_notebook_to_trash(path: String) -> Result<(), Error> {
    let path = normalize_path(Path::new(&path)).await?;
    let current_path = load_current_notebook_path_normalized().await?;
    move_notebook_to_trash_with_current_path(&path, current_path.as_deref(), trash_path)
}

fn move_notebook_to_trash_with_current_path<F>(
    path: &Path,
    current_path: Option<&Path>,
    trasher: F,
) -> Result<(), Error>
where
    F: FnOnce(&Path) -> Result<(), Error>,
{
    if current_path == Some(path) {
        return Err(Error::NotebookDaemon(
            "cannot move the currently loaded notebook to trash".to_string(),
        ));
    }
    trasher(path)
}

fn trash_path(path: &Path) -> Result<(), Error> {
    trash::delete(path)
        .map_err(|error| Error::Filesystem(io::Error::new(io::ErrorKind::Other, error.to_string())))
}

/// Reveal a notebook in the platform file manager.
#[tauri::command]
pub async fn reveal_notebook_in_finder(path: String) -> Result<(), Error> {
    reveal_notebook_path(Path::new(&path)).await
}

async fn reveal_notebook_path(path: &Path) -> Result<(), Error> {
    let mut command = reveal_command(path)?;
    let status = command.status().await.map_err(Error::Subprocess)?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::Subprocess(io::Error::new(
            io::ErrorKind::Other,
            format!("file manager exited with status {status}"),
        )))
    }
}

fn reveal_command(path: &Path) -> Result<tokio::process::Command, Error> {
    #[cfg(target_os = "macos")]
    {
        let mut command = tokio::process::Command::new("open");
        command.arg("-R").arg(path);
        return Ok(command);
    }

    #[cfg(target_os = "windows")]
    {
        let mut command = tokio::process::Command::new("explorer");
        command.arg(format!("/select,{}", path.display()));
        return Ok(command);
    }

    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let parent = path.parent().ok_or_else(|| {
            Error::Filesystem(io::Error::new(
                io::ErrorKind::InvalidInput,
                "notebook path must have a parent directory",
            ))
        })?;
        let mut command = tokio::process::Command::new("xdg-open");
        command.arg(parent);
        Ok(command)
    }
}

/// Move all inactive scratch notebooks to the OS trash.
#[tauri::command]
pub async fn discard_scratch_notebooks() -> Result<usize, Error> {
    let scratch_dir = scratch_dir()?;
    let current_path = load_current_notebook_path_normalized().await?;
    discard_scratch_notebooks_in(&scratch_dir, current_path.as_deref(), trash_path).await
}

async fn discard_scratch_notebooks_in<F>(
    scratch_dir: &Path,
    current_path: Option<&Path>,
    mut trasher: F,
) -> Result<usize, Error>
where
    F: FnMut(&Path) -> Result<(), Error>,
{
    let mut entries = match tokio::fs::read_dir(scratch_dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(Error::Filesystem(error)),
    };

    let mut trashed = 0;
    while let Some(entry) = entries.next_entry().await.map_err(Error::Filesystem)? {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("ipynb") {
            continue;
        }
        let path = normalize_path(&path).await?;
        if current_path == Some(path.as_path()) {
            continue;
        }
        trasher(&path)?;
        trashed += 1;
    }
    Ok(trashed)
}

/// Mark the frontend agent bridge listener as registered.
///
/// The standalone Jute shell has no agent transport; this no-op command keeps
/// the shared frontend boot path compatible with the SPUR notebook binary.
#[tauri::command]
pub async fn bridge_ready() -> Result<(), Error> {
    Ok(())
}

/// Accept a frontend agent bridge response without forwarding it.
///
/// The SPUR notebook binary registers the real transport command. Standalone
/// Jute only needs this stub so unresolved bridge invocations do not fail.
#[tauri::command]
pub async fn agent_response(payload: Value) -> Result<(), Error> {
    let _ = payload;
    Ok(())
}

/// Track whether a notebook page is active in the frontend.
///
/// Standalone Jute does not expose notebook state to an agent bridge, so this
/// command intentionally records nothing.
#[tauri::command]
pub async fn notebook_active_changed(open: bool) -> Result<(), Error> {
    let _ = open;
    Ok(())
}

/// Start a new Jupyter kernel.
#[tauri::command]
pub async fn start_kernel(
    app: AppHandle,
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

    crate::kernel_provision::ensure_python3_kernelspec(&app).await?;
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
            Arc, Mutex as StdMutex,
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
                    spur: Some(SpurCellMetadata {
                        version,
                        last_edited_by: None,
                    }),
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

    #[test]
    fn autosave_panic_before_atomic_rename_leaves_ipynb_fully_old_or_new() {
        let dir = std::env::temp_dir().join(format!("jute-save-panic-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notebook.ipynb");

        atomic_write_notebook_blocking(&path, &notebook_with_source("old", 1)).unwrap();

        let panic_result = std::panic::catch_unwind(|| {
            atomic_write_notebook_blocking_with_hook(
                &path,
                &notebook_with_source("new", 2),
                |_temp_path| panic!("simulated JS panic mid-debounce before atomic rename"),
            )
            .unwrap();
        });
        assert!(panic_result.is_err());

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: NotebookRoot = serde_json::from_str(&contents).unwrap();
        let source = first_source(&parsed);
        assert!(
            source == "old" || source == "new",
            "autosave target must be a complete old or new notebook, got {source:?}"
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn move_notebook_to_trash_refuses_current_notebook() {
        let path = std::env::temp_dir()
            .join(format!("jute-trash-current-{}", Uuid::new_v4()))
            .join("active.ipynb");

        let result = move_notebook_to_trash_with_current_path(&path, Some(path.as_path()), |_| {
            panic!("active notebook must not be trashed");
        });

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn discard_scratch_notebooks_skips_active_notebook() {
        let scratch_dir = std::env::temp_dir().join(format!("jute-scratch-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&scratch_dir).unwrap();
        let active = scratch_dir.join("active.ipynb");
        let stale = scratch_dir.join("stale.ipynb");
        let non_notebook = scratch_dir.join("note.txt");
        std::fs::write(&active, b"{}").unwrap();
        std::fs::write(&stale, b"{}").unwrap();
        std::fs::write(&non_notebook, b"not a notebook").unwrap();
        let active = normalize_path(&active).await.unwrap();
        let stale = normalize_path(&stale).await.unwrap();

        let trashed = Arc::new(StdMutex::new(Vec::<PathBuf>::new()));
        let count = discard_scratch_notebooks_in(&scratch_dir, Some(active.as_path()), {
            let trashed = Arc::clone(&trashed);
            move |path: &Path| {
                trashed.lock().unwrap().push(path.to_path_buf());
                Ok(())
            }
        })
        .await
        .unwrap();

        assert_eq!(count, 1);
        assert_eq!(trashed.lock().unwrap().as_slice(), [stale]);

        std::fs::remove_dir_all(scratch_dir).unwrap();
    }

    #[tokio::test]
    async fn agent_bridge_stub_commands_accept_payload_without_transport() {
        bridge_ready().await.unwrap();
        notebook_active_changed(true).await.unwrap();
        agent_response(serde_json::json!({
            "requestId": "550e8400-e29b-41d4-a716-446655440000",
            "result": { "ok": true }
        }))
        .await
        .unwrap();
    }
}
