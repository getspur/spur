//! Authoritative in-memory notebook document store.

use std::{
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Weak,
    },
    time::Duration,
};

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::sync::{broadcast, Mutex as AsyncMutex, Notify};
use ts_rs::TS;
use uuid::Uuid;

use crate::{
    backend::{
        commands::RunCellEvent,
        notebook::{
            Cell, CellDagMetadata, CellMetadata, CodeCell, CodeType, FrontendCellMetadata,
            JuteDeckCellMetadata, MarkdownCell, MultilineString, NotebookMetadata, NotebookRoot,
            Output, OutputDisplayData, OutputError, OutputExecuteResult, OutputStream, RawCell,
            SpurCellMetadata,
        },
    },
    commands::SaveCoordinator,
    Error as JuteError,
};

const AUTOSAVE_DEBOUNCE: Duration = Duration::from_millis(750);

/// Authoritative Rust-owned notebook document store.
pub struct NotebookStore {
    inner: Arc<RwLock<NotebookRoot>>,
    version: AtomicU64,
    dirty: AtomicBool,
    broadcast: broadcast::Sender<NotebookDelta>,
    save_coord: Arc<SaveCoordinator>,
    path: Mutex<Option<PathBuf>>,
    flush_lock: AsyncMutex<()>,
    autosave_notify: Arc<Notify>,
    autosave_task_started: AtomicBool,
    self_ref: Weak<Self>,
}

/// Cell kind to create when inserting a notebook cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case")]
pub enum CellKind {
    /// Raw cell.
    Raw,
    /// Markdown cell.
    Markdown,
    /// Code cell.
    Code,
}

/// Mutation operations accepted by [`NotebookStore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotebookOp {
    /// Replace the source of an existing cell, optionally checking cell version.
    WriteCell {
        /// Cell identifier.
        id: String,
        /// New cell source.
        source: String,
        /// Expected cell version for optimistic concurrency.
        expected_version: Option<u64>,
        /// Agent that last edited the cell.
        last_edited_by: Option<String>,
    },
    /// Insert a new cell after an optional existing cell.
    InsertCell {
        /// Kind of cell to insert.
        kind: CellKind,
        /// Existing cell identifier to insert after.
        after_id: Option<String>,
        /// Initial cell source.
        source: String,
        /// Agent that created the cell.
        last_edited_by: Option<String>,
        /// Optional code language metadata for code cells.
        code_type: Option<CodeType>,
    },
    /// Delete an existing cell after checking cell version.
    DeleteCell {
        /// Cell identifier.
        id: String,
        /// Expected cell version for optimistic concurrency.
        expected_version: u64,
    },
    /// Merge jute-deck metadata for an existing cell after checking cell version.
    SetJuteDeckMetadata {
        /// Cell identifier.
        id: String,
        /// Metadata patch. Only `Some` fields overwrite existing values.
        patch: JuteDeckCellMetadata,
        /// Expected cell version for optimistic concurrency.
        expected_version: u64,
    },
    /// Set SPUR DAG metadata for an existing cell after checking cell version.
    SetSpurDagMetadata {
        /// Cell identifier.
        id: String,
        /// DAG metadata patch.
        patch: CellDagMetadata,
        /// Expected cell version for optimistic concurrency.
        expected_version: u64,
    },
    /// Set SPUR code type metadata for an existing cell after checking cell version.
    SetSpurCodeTypeMetadata {
        /// Cell identifier.
        id: String,
        /// Code type metadata patch.
        code_type: CodeType,
        /// Expected cell version for optimistic concurrency.
        expected_version: u64,
    },
    /// Set SPUR frontend-cell metadata for an existing cell after checking cell version.
    SetSpurFrontendMetadata {
        /// Cell identifier.
        id: String,
        /// Frontend-cell metadata patch.
        patch: FrontendCellMetadata,
        /// Expected cell version for optimistic concurrency.
        expected_version: u64,
    },
    /// Replace the source of an existing cell without concurrency checks.
    ApplyEdit {
        /// Cell identifier.
        id: String,
        /// New cell source.
        source: String,
    },
    /// Mark a cell as the SPUR-managed datasource setup cell.
    MarkDatasourceSetupCell {
        /// Cell identifier.
        id: String,
        /// Expected cell version for optimistic concurrency.
        expected_version: u64,
    },
}

/// Broadcast notification emitted after each store mutation.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct NotebookDelta {
    /// Monotonic document version after the mutation.
    #[ts(type = "number")]
    pub version: u64,
    /// Worktree path of the notebook this delta belongs to. `None` only when the
    /// store has no loaded path yet (a fresh/unsaved store). Used by the frontend
    /// to drop deltas that belong to a different open notebook window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    /// Kind of mutation represented by this delta.
    pub kind: DeltaKind,
}

/// Kind of store mutation represented by a [`NotebookDelta`].
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub enum DeltaKind {
    /// A cell's source was replaced.
    CellWritten {
        /// Post-mutation cell, self-contained so the reducer needs no refetch.
        cell: DaemonCell,
    },
    /// A new cell was inserted.
    CellInserted {
        /// Post-mutation inserted cell, self-contained so the reducer needs no refetch.
        cell: DaemonCell,
        /// Existing cell identifier the new cell was inserted after.
        after_id: Option<String>,
    },
    /// A cell was deleted.
    CellDeleted {
        /// Cell identifier.
        id: String,
    },
    /// A run-cell event was applied to a code cell.
    RunCellEvent {
        /// Cell identifier.
        cell_id: String,
        /// Applied run event.
        #[serde(skip_deserializing, default = "default_run_cell_event")]
        event: RunCellEvent,
    },
    /// The reactive DAG status snapshot changed.
    DagStatusChanged {
        /// Full DAG status snapshot for frontend reducers.
        snapshot: Value,
    },
    /// A notebook was loaded into the store.
    Loaded {
        /// Full notebook root after the load, self-contained for the reducer.
        root: NotebookRoot,
    },
}

/// Self-contained cell payload carried by deltas and daemon control reads.
#[expect(missing_docs)]
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct DaemonCell {
    pub id: String,
    pub kind: String,
    #[ts(type = "number")]
    pub version: u64,
    pub last_edited_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub datasource_setup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub dag_metadata: Option<CellDagMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code_type: Option<CodeType>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub frontend_metadata: Option<FrontendCellMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub jute_deck_metadata: Option<JuteDeckCellMetadata>,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    #[ts(skip)]
    pub metadata_other: Map<String, Value>,
    pub source: String,
    pub exec_count: Option<u32>,
    pub status: String,
    pub outputs: Vec<Value>,
}

/// Pending delta kind captured under the write lock before the version bump.
///
/// Self-contained variants (e.g. delete) carry their final payload immediately;
/// cell-bearing variants only remember the cell id so the inline [`DaemonCell`]
/// can be built from the post-mutation document after the version + metadata
/// update is applied.
enum PendingDelta {
    Written {
        id: String,
    },
    Inserted {
        id: String,
        after_id: Option<String>,
    },
    Deleted {
        id: String,
    },
}

/// Error returned by notebook store mutations.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    /// The caller's expected version did not match the actual store version.
    #[error(
        "optimistic concurrency check failed: expected version {expected}, actual version {actual}"
    )]
    OptimisticConcurrency {
        /// Version expected by the caller.
        expected: u64,
        /// Current actual store version.
        actual: u64,
    },
    /// No cell with the requested identifier exists.
    #[error("cell not found: {id}")]
    CellNotFound {
        /// Missing cell identifier.
        id: String,
    },
    /// A run event targeted a cell that is not a code cell.
    #[error("cell is not a code cell: {id}")]
    NotCodeCell {
        /// Non-code cell identifier.
        id: String,
    },
}

impl NotebookStore {
    /// Create a new notebook store.
    pub fn new(save_coord: Arc<SaveCoordinator>) -> Arc<Self> {
        let (broadcast, _receiver) = broadcast::channel(128);
        Arc::new_cyclic(|self_ref| Self {
            inner: Arc::new(RwLock::new(empty_notebook())),
            version: AtomicU64::new(0),
            dirty: AtomicBool::new(false),
            broadcast,
            save_coord,
            path: Mutex::new(None),
            flush_lock: AsyncMutex::new(()),
            autosave_notify: Arc::new(Notify::new()),
            autosave_task_started: AtomicBool::new(false),
            self_ref: Weak::clone(self_ref),
        })
    }

    /// Load a notebook document into the store.
    pub fn load<P>(&self, path: P, root: NotebookRoot) -> NotebookDelta
    where
        P: Into<PathBuf>,
    {
        let (version, root_snapshot) = {
            let mut stored_path = self.path.lock();
            let mut inner = self.inner.write();
            *inner = root;
            *stored_path = Some(path.into());
            self.dirty.store(false, Ordering::SeqCst);
            let version = self.bump_version();
            (version, inner.clone())
        };

        let delta = self.make_delta(
            version,
            DeltaKind::Loaded {
                root: root_snapshot,
            },
        );
        self.publish(&delta);
        delta
    }

    /// Replace the current notebook document and mark it dirty for persistence.
    pub fn replace<P: Into<PathBuf>>(&self, path: P, root: NotebookRoot) -> NotebookDelta {
        let (version, root_snapshot) = {
            let mut stored_path = self.path.lock();
            let mut inner = self.inner.write();
            *inner = root;
            *stored_path = Some(path.into());
            let version = self.bump_version();
            (version, inner.clone())
        };

        let delta = self.make_delta(
            version,
            DeltaKind::Loaded {
                root: root_snapshot,
            },
        );
        self.mark_dirty();
        self.publish(&delta);
        delta
    }

    /// Return a point-in-time notebook snapshot and version.
    pub fn snapshot(&self) -> (NotebookRoot, u64) {
        let inner = self.inner.read();
        let version = self.version.load(Ordering::SeqCst);
        (inner.clone(), version)
    }

    /// Return the path of the loaded notebook, if one has been loaded.
    pub fn path(&self) -> Option<PathBuf> {
        self.path.lock().clone()
    }

    /// Build a delta stamped with the store's current notebook path.
    fn make_delta(&self, version: u64, kind: DeltaKind) -> NotebookDelta {
        NotebookDelta {
            version,
            path: self.path().map(|path| path.display().to_string()),
            kind,
        }
    }

    #[cfg(test)]
    fn is_dirty_for_test(&self) -> bool {
        self.dirty.load(Ordering::SeqCst)
    }

    /// Check that an existing cell's SPUR metadata version matches the caller's expectation.
    pub fn check_cell_version(&self, cell_id: &str, expected: u64) -> Result<(), StoreError> {
        let root = self.inner.read();
        Self::ensure_cell_version(&root, cell_id, expected)
    }

    /// Apply a notebook edit operation.
    pub fn apply(&self, op: NotebookOp) -> Result<NotebookDelta, StoreError> {
        let mut root = self.inner.write();
        let (pending, metadata_update) = match op {
            NotebookOp::WriteCell {
                id,
                source,
                expected_version,
                last_edited_by,
            } => {
                if let Some(expected) = expected_version {
                    Self::ensure_cell_version(&root, &id, expected)?;
                }
                let cell = find_cell_mut(&mut root, &id)
                    .ok_or_else(|| StoreError::CellNotFound { id: id.clone() })?;
                set_cell_source(cell, source);
                let metadata_update = Some((id.clone(), last_edited_by));
                (PendingDelta::Written { id }, metadata_update)
            }
            NotebookOp::InsertCell {
                kind,
                after_id,
                source,
                last_edited_by,
                code_type,
            } => {
                let insert_at = match after_id.as_deref() {
                    Some(after_id) => find_cell_index(&root, after_id)
                        .map(|index| index + 1)
                        .ok_or_else(|| StoreError::CellNotFound {
                            id: after_id.to_owned(),
                        })?,
                    None => root.cells.len(),
                };
                let id = Uuid::new_v4().to_string();
                root.cells
                    .insert(insert_at, make_cell(kind, id.clone(), source, code_type));
                let metadata_update = Some((id.clone(), last_edited_by));
                (PendingDelta::Inserted { id, after_id }, metadata_update)
            }
            NotebookOp::DeleteCell {
                id,
                expected_version,
            } => {
                Self::ensure_cell_version(&root, &id, expected_version)?;
                let index = find_cell_index(&root, &id)
                    .ok_or_else(|| StoreError::CellNotFound { id: id.clone() })?;
                root.cells.remove(index);
                (PendingDelta::Deleted { id }, None)
            }
            NotebookOp::SetJuteDeckMetadata {
                id,
                patch,
                expected_version,
            } => {
                Self::ensure_cell_version(&root, &id, expected_version)?;
                let cell = find_cell_mut(&mut root, &id)
                    .ok_or_else(|| StoreError::CellNotFound { id: id.clone() })?;
                let metadata = cell_metadata_mut(cell);
                let mut merged = metadata.jute_deck.clone().unwrap_or_default();
                merge_jute_deck_metadata(&mut merged, patch);
                metadata.jute_deck = Some(merged);
                let metadata_update = Some((id.clone(), Some("brain".to_owned())));
                (PendingDelta::Written { id }, metadata_update)
            }
            NotebookOp::SetSpurDagMetadata {
                id,
                patch,
                expected_version,
            } => {
                Self::ensure_cell_version(&root, &id, expected_version)?;
                let cell = find_cell_mut(&mut root, &id)
                    .ok_or_else(|| StoreError::CellNotFound { id: id.clone() })?;
                let metadata = cell_metadata_mut(cell);
                let spur = metadata.spur.get_or_insert_with(empty_spur_cell_metadata);
                spur.dag = Some(patch);
                let metadata_update = Some((id.clone(), Some("brain".to_owned())));
                (PendingDelta::Written { id }, metadata_update)
            }
            NotebookOp::SetSpurCodeTypeMetadata {
                id,
                code_type,
                expected_version,
            } => {
                Self::ensure_cell_version(&root, &id, expected_version)?;
                let cell = find_cell_mut(&mut root, &id)
                    .ok_or_else(|| StoreError::CellNotFound { id: id.clone() })?;
                let metadata = cell_metadata_mut(cell);
                let spur = metadata.spur.get_or_insert_with(empty_spur_cell_metadata);
                spur.code_type = Some(code_type);
                let metadata_update = Some((id.clone(), Some("brain".to_owned())));
                (PendingDelta::Written { id }, metadata_update)
            }
            NotebookOp::SetSpurFrontendMetadata {
                id,
                patch,
                expected_version,
            } => {
                Self::ensure_cell_version(&root, &id, expected_version)?;
                let cell = find_cell_mut(&mut root, &id)
                    .ok_or_else(|| StoreError::CellNotFound { id: id.clone() })?;
                let metadata = cell_metadata_mut(cell);
                let spur = metadata.spur.get_or_insert_with(empty_spur_cell_metadata);
                spur.frontend = Some(patch);
                let metadata_update = Some((id.clone(), Some("brain".to_owned())));
                (PendingDelta::Written { id }, metadata_update)
            }
            NotebookOp::ApplyEdit { id, source } => {
                let cell = find_cell_mut(&mut root, &id)
                    .ok_or_else(|| StoreError::CellNotFound { id: id.clone() })?;
                set_cell_source(cell, source);
                let metadata_update = Some((id.clone(), None));
                (PendingDelta::Written { id }, metadata_update)
            }
            NotebookOp::MarkDatasourceSetupCell {
                id,
                expected_version,
            } => {
                Self::ensure_cell_version(&root, &id, expected_version)?;
                let cell = find_cell_mut(&mut root, &id)
                    .ok_or_else(|| StoreError::CellNotFound { id: id.clone() })?;
                let metadata = cell_metadata_mut(cell);
                let spur = metadata.spur.get_or_insert_with(empty_spur_cell_metadata);
                spur.datasource_setup = Some(true);
                let metadata_update = Some((id.clone(), None));
                (PendingDelta::Written { id }, metadata_update)
            }
        };

        // Two-phase: bump the version and stamp the spur metadata first, then
        // build the inline cell from the now-final document so its version and
        // last_edited_by match the delta.
        let version = self.bump_version();
        if let Some((id, last_edited_by)) = metadata_update {
            if let Some(cell) = find_cell_mut(&mut root, &id) {
                set_cell_spur_metadata(cell, version, last_edited_by);
            }
        }

        let kind = match pending {
            PendingDelta::Written { id } => DeltaKind::CellWritten {
                cell: daemon_cell(&root, &id).ok_or(StoreError::CellNotFound { id })?,
            },
            PendingDelta::Inserted { id, after_id } => DeltaKind::CellInserted {
                cell: daemon_cell(&root, &id).ok_or(StoreError::CellNotFound { id })?,
                after_id,
            },
            PendingDelta::Deleted { id } => DeltaKind::CellDeleted { id },
        };

        let delta = self.make_delta(version, kind);
        self.mark_dirty();
        drop(root);
        self.publish(&delta);
        Ok(delta)
    }

    /// Apply a kernel run event to a code cell.
    pub fn apply_run_event(
        &self,
        cell_id: impl Into<String>,
        event: RunCellEvent,
    ) -> Result<NotebookDelta, StoreError> {
        let cell_id = cell_id.into();
        let mut root = self.inner.write();
        let cell = find_cell_mut(&mut root, &cell_id).ok_or_else(|| StoreError::CellNotFound {
            id: cell_id.clone(),
        })?;
        let Cell::Code(code_cell) = cell else {
            return Err(StoreError::NotCodeCell { id: cell_id });
        };
        apply_event_to_code_cell(code_cell, &event);

        let delta = self.make_delta(
            self.bump_version(),
            DeltaKind::RunCellEvent { cell_id, event },
        );
        self.mark_dirty();
        drop(root);
        self.publish(&delta);
        Ok(delta)
    }

    /// Flush the current notebook contents to disk through the save coordinator.
    pub async fn flush(&self) -> Result<(), io::Error> {
        let _guard = self.flush_lock.lock().await;
        if !self.dirty.load(Ordering::SeqCst) {
            return Ok(());
        }

        let path = self.path.lock().clone().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot flush notebook store before a path is loaded",
            )
        })?;
        let (snapshot, saved_version) = self.snapshot();
        self.save_coord
            .save(path, snapshot)
            .await
            .map_err(save_error_to_io)?;

        if self.version.load(Ordering::SeqCst) == saved_version {
            self.dirty.store(false, Ordering::SeqCst);
        }
        Ok(())
    }

    /// Subscribe to notebook deltas.
    pub fn subscribe(&self) -> broadcast::Receiver<NotebookDelta> {
        self.broadcast.subscribe()
    }

    /// Publish a reactive DAG status snapshot on the notebook delta channel.
    pub fn publish_dag_status_changed(&self, snapshot: Value) -> NotebookDelta {
        let delta = self.make_delta(
            self.version.load(Ordering::SeqCst),
            DeltaKind::DagStatusChanged { snapshot },
        );
        self.publish(&delta);
        delta
    }

    fn ensure_cell_version(
        root: &NotebookRoot,
        cell_id: &str,
        expected: u64,
    ) -> Result<(), StoreError> {
        let cell = find_cell(root, cell_id).ok_or_else(|| StoreError::CellNotFound {
            id: cell_id.to_owned(),
        })?;
        let actual = cell_spur_version(cell).unwrap_or_default();
        if expected == actual {
            Ok(())
        } else {
            Err(StoreError::OptimisticConcurrency { expected, actual })
        }
    }

    fn bump_version(&self) -> u64 {
        self.version.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::SeqCst);
        self.schedule_autosave();
    }

    fn schedule_autosave(&self) {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };

        if self
            .autosave_task_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let store = Weak::clone(&self.self_ref);
            let notify = Arc::clone(&self.autosave_notify);
            drop(handle.spawn(async move {
                autosave_loop(store, notify).await;
            }));
        }

        self.autosave_notify.notify_one();
    }

    fn publish(&self, delta: &NotebookDelta) {
        let _ = self.broadcast.send(delta.clone());
    }
}

impl Drop for NotebookStore {
    fn drop(&mut self) {
        self.autosave_notify.notify_waiters();
    }
}

async fn autosave_loop(store: Weak<NotebookStore>, notify: Arc<Notify>) {
    loop {
        notify.notified().await;
        if store.upgrade().is_none() {
            return;
        }

        loop {
            tokio::select! {
                () = tokio::time::sleep(AUTOSAVE_DEBOUNCE) => {
                    let Some(store) = store.upgrade() else {
                        return;
                    };
                    if let Err(error) = store.flush().await {
                        tracing::warn!(%error, "failed to autosave notebook store");
                    }
                    break;
                }
                () = notify.notified() => {
                    if store.upgrade().is_none() {
                        return;
                    }
                }
            }
        }
    }
}

fn empty_notebook() -> NotebookRoot {
    NotebookRoot {
        metadata: NotebookMetadata {
            kernelspec: None,
            language_info: None,
            orig_nbformat: None,
            title: None,
            authors: None,
            jute_deck: None,
            other: Map::new(),
        },
        nbformat_minor: 5,
        nbformat: 4,
        cells: Vec::new(),
    }
}

fn make_cell(kind: CellKind, id: String, source: String, code_type: Option<CodeType>) -> Cell {
    match kind {
        CellKind::Raw => Cell::Raw(RawCell {
            id: Some(id),
            metadata: empty_cell_metadata(),
            source: MultilineString::Single(source),
            attachments: None,
        }),
        CellKind::Markdown => Cell::Markdown(MarkdownCell {
            id: Some(id),
            metadata: empty_cell_metadata(),
            source: MultilineString::Single(source),
            attachments: None,
        }),
        CellKind::Code => Cell::Code(CodeCell {
            id: Some(id),
            metadata: cell_metadata_with_code_type(code_type),
            source: MultilineString::Single(source),
            execution_count: None,
            outputs: Vec::new(),
        }),
    }
}

fn empty_cell_metadata() -> CellMetadata {
    CellMetadata {
        spur: None,
        jute_deck: None,
        other: Map::new(),
    }
}

fn cell_metadata_with_code_type(code_type: Option<CodeType>) -> CellMetadata {
    let Some(code_type) = code_type else {
        return empty_cell_metadata();
    };
    CellMetadata {
        spur: Some(SpurCellMetadata {
            code_type: Some(code_type),
            ..empty_spur_cell_metadata()
        }),
        jute_deck: None,
        other: Map::new(),
    }
}

fn empty_spur_cell_metadata() -> SpurCellMetadata {
    SpurCellMetadata {
        version: 0,
        last_edited_by: None,
        datasource_setup: None,
        dag: None,
        code_type: None,
        frontend: None,
    }
}

fn find_cell_index(root: &NotebookRoot, id: &str) -> Option<usize> {
    root.cells
        .iter()
        .position(|cell| cell_id(cell).is_some_and(|cell_id| cell_id == id))
}

fn find_cell<'a>(root: &'a NotebookRoot, id: &str) -> Option<&'a Cell> {
    root.cells
        .iter()
        .find(|cell| cell_id(cell).is_some_and(|cell_id| cell_id == id))
}

/// Build a self-contained [`DaemonCell`] from the current document, or `None`
/// when no cell with `id` exists. This is the layering-correct home for the
/// root→cell conversion; `commands` re-exports it for the daemon read path.
pub(crate) fn daemon_cell(root: &NotebookRoot, id: &str) -> Option<DaemonCell> {
    let cell = find_cell(root, id)?;
    let metadata = cell_metadata(cell);
    let spur = metadata.spur.as_ref();
    let (kind, source, exec_count, outputs) = match cell {
        Cell::Raw(cell) => ("raw", multiline_to_string(&cell.source), None, Vec::new()),
        Cell::Markdown(cell) => (
            "markdown",
            multiline_to_string(&cell.source),
            None,
            Vec::new(),
        ),
        Cell::Code(cell) => (
            "code",
            multiline_to_string(&cell.source),
            cell.execution_count,
            cell.outputs
                .iter()
                .map(|output| serde_json::to_value(output).unwrap_or(Value::Null))
                .collect(),
        ),
    };

    Some(DaemonCell {
        id: id.to_owned(),
        kind: kind.to_owned(),
        version: spur.map(|spur| spur.version).unwrap_or_default(),
        last_edited_by: spur.and_then(|spur| spur.last_edited_by.clone()),
        datasource_setup: spur.and_then(|spur| spur.datasource_setup),
        dag_metadata: spur.and_then(|spur| spur.dag.clone()),
        code_type: spur.and_then(|spur| spur.code_type),
        frontend_metadata: spur.and_then(|spur| spur.frontend.clone()),
        jute_deck_metadata: metadata.jute_deck.clone(),
        metadata_other: metadata.other.clone(),
        source,
        exec_count,
        status: "idle".to_owned(),
        outputs,
    })
}

/// Merge authoritative Rust-owned SPUR metadata into notebook contents supplied
/// by a frontend export without replacing live frontend-owned cell source or
/// outputs. This protects GUI save paths from stale/lossy frontend metadata.
pub(crate) fn merge_authoritative_spur_metadata_for_save(
    contents: &mut NotebookRoot,
    authoritative: &NotebookRoot,
) {
    for target_cell in &mut contents.cells {
        let Some(id) = cell_id(target_cell).map(str::to_owned) else {
            continue;
        };
        let Some(authoritative_cell) = find_cell(authoritative, &id) else {
            continue;
        };
        let Some(authoritative_spur) = cell_metadata(authoritative_cell).spur.as_ref() else {
            continue;
        };

        let target_metadata = cell_metadata_mut(target_cell);
        let target_spur = target_metadata.spur.get_or_insert_with(|| {
            crate::backend::notebook::SpurCellMetadata {
                version: authoritative_spur.version,
                last_edited_by: authoritative_spur.last_edited_by.clone(),
                datasource_setup: authoritative_spur.datasource_setup,
                dag: None,
                code_type: authoritative_spur.code_type,
                frontend: authoritative_spur.frontend.clone(),
            }
        });

        if let Some(datasource_setup) = authoritative_spur.datasource_setup {
            target_spur.datasource_setup = Some(datasource_setup);
        }
        if let Some(dag) = authoritative_spur.dag.clone() {
            target_spur.dag = Some(dag);
        }
        if let Some(code_type) = authoritative_spur.code_type {
            target_spur.code_type = Some(code_type);
        }
        if let Some(frontend) = authoritative_spur.frontend.clone() {
            target_spur.frontend = Some(frontend);
        }
        if target_spur.last_edited_by.is_none() {
            target_spur.last_edited_by = authoritative_spur.last_edited_by.clone();
        }
    }
}

fn multiline_to_string(source: &MultilineString) -> String {
    match source {
        MultilineString::Single(source) => source.clone(),
        MultilineString::Multi(lines) => lines.join(""),
    }
}

fn find_cell_mut<'a>(root: &'a mut NotebookRoot, id: &str) -> Option<&'a mut Cell> {
    root.cells
        .iter_mut()
        .find(|cell| cell_id(cell).is_some_and(|cell_id| cell_id == id))
}

fn cell_metadata(cell: &Cell) -> &CellMetadata {
    match cell {
        Cell::Raw(cell) => &cell.metadata,
        Cell::Markdown(cell) => &cell.metadata,
        Cell::Code(cell) => &cell.metadata,
    }
}

fn cell_id(cell: &Cell) -> Option<&str> {
    match cell {
        Cell::Raw(cell) => cell.id.as_deref(),
        Cell::Markdown(cell) => cell.id.as_deref(),
        Cell::Code(cell) => cell.id.as_deref(),
    }
}

fn cell_spur_version(cell: &Cell) -> Option<u64> {
    match cell {
        Cell::Raw(cell) => cell.metadata.spur.as_ref().map(|spur| spur.version),
        Cell::Markdown(cell) => cell.metadata.spur.as_ref().map(|spur| spur.version),
        Cell::Code(cell) => cell.metadata.spur.as_ref().map(|spur| spur.version),
    }
}

fn set_cell_source(cell: &mut Cell, source: String) {
    let source = MultilineString::Single(source);
    match cell {
        Cell::Raw(cell) => cell.source = source,
        Cell::Markdown(cell) => cell.source = source,
        Cell::Code(cell) => cell.source = source,
    }
}

fn cell_metadata_mut(cell: &mut Cell) -> &mut CellMetadata {
    match cell {
        Cell::Raw(cell) => &mut cell.metadata,
        Cell::Markdown(cell) => &mut cell.metadata,
        Cell::Code(cell) => &mut cell.metadata,
    }
}

fn merge_jute_deck_metadata(metadata: &mut JuteDeckCellMetadata, patch: JuteDeckCellMetadata) {
    if patch.layout.is_some() {
        metadata.layout = patch.layout;
    }
    if patch.hidden.is_some() {
        metadata.hidden = patch.hidden;
    }
    if patch.speaker_notes.is_some() {
        metadata.speaker_notes = patch.speaker_notes;
    }
    if patch.theme_override.is_some() {
        metadata.theme_override = patch.theme_override;
    }
    if patch.fragments.is_some() {
        metadata.fragments = patch.fragments;
    }
    if patch.background.is_some() {
        metadata.background = patch.background;
    }
}

fn set_cell_spur_metadata(cell: &mut Cell, version: u64, last_edited_by: Option<String>) {
    let metadata = match cell {
        Cell::Raw(cell) => &mut cell.metadata,
        Cell::Markdown(cell) => &mut cell.metadata,
        Cell::Code(cell) => &mut cell.metadata,
    };
    let previous_last_edited_by = metadata
        .spur
        .as_ref()
        .and_then(|spur| spur.last_edited_by.clone());
    let previous_datasource_setup = metadata
        .spur
        .as_ref()
        .and_then(|spur| spur.datasource_setup);
    let previous_dag = metadata.spur.as_ref().and_then(|spur| spur.dag.clone());
    let previous_code_type = metadata.spur.as_ref().and_then(|spur| spur.code_type);
    let previous_frontend = metadata
        .spur
        .as_ref()
        .and_then(|spur| spur.frontend.clone());
    metadata.spur = Some(crate::backend::notebook::SpurCellMetadata {
        version,
        last_edited_by: last_edited_by.or(previous_last_edited_by),
        datasource_setup: previous_datasource_setup,
        dag: previous_dag,
        code_type: previous_code_type,
        frontend: previous_frontend,
    });
}

fn default_run_cell_event() -> RunCellEvent {
    RunCellEvent::Started
}

fn apply_event_to_code_cell(cell: &mut CodeCell, event: &RunCellEvent) {
    match event {
        RunCellEvent::Started => {
            cell.execution_count = None;
            cell.outputs.clear();
        }
        RunCellEvent::Stdout(text) => cell.outputs.push(stream_output("stdout", text)),
        RunCellEvent::Stderr(text) => cell.outputs.push(stream_output("stderr", text)),
        RunCellEvent::ExecuteResult(result) => {
            cell.outputs
                .push(Output::ExecuteResult(OutputExecuteResult {
                    execution_count: u32::try_from(result.execution_count).ok(),
                    data: result.data.clone(),
                    metadata: result.metadata.clone(),
                    other: Map::new(),
                }));
        }
        RunCellEvent::DisplayData(data) | RunCellEvent::UpdateDisplayData(data) => {
            cell.outputs.push(Output::DisplayData(OutputDisplayData {
                data: data.data.clone(),
                metadata: data.metadata.clone(),
                other: Map::new(),
            }));
        }
        RunCellEvent::ClearOutput(_) => cell.outputs.clear(),
        RunCellEvent::Error(error) => {
            cell.outputs.push(Output::Error(OutputError {
                ename: error.ename.clone(),
                evalue: error.evalue.clone(),
                traceback: error.traceback.clone(),
                other: Map::new(),
            }));
        }
        RunCellEvent::Disconnect(message) => {
            cell.outputs.push(Output::Error(OutputError {
                ename: "KernelDisconnect".to_owned(),
                evalue: message.clone(),
                traceback: Vec::new(),
                other: Map::new(),
            }));
        }
        RunCellEvent::Finished { exec_count, .. } => {
            cell.execution_count = *exec_count;
        }
        RunCellEvent::CompileProgress { .. }
        | RunCellEvent::CommOpen(_)
        | RunCellEvent::CommMsg(_)
        | RunCellEvent::CommClose(_) => {}
    }
}

fn stream_output(name: &str, text: &str) -> Output {
    Output::Stream(OutputStream {
        name: name.to_owned(),
        text: MultilineString::Single(text.to_owned()),
        other: Map::new(),
    })
}

fn save_error_to_io(error: JuteError) -> io::Error {
    match error {
        JuteError::Filesystem(error) => error,
        error => io::Error::other(error),
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use futures_util::future::join_all;
    use serde_json::Map;
    use tokio::time::timeout;

    use super::*;
    use crate::backend::notebook::{
        Cell, CellDagMetadata, CellMetadata, CodeCell, CodeType, DagSource, JuteDeckCellMetadata,
        JuteDeckLayout, MultilineString, NotebookMetadata, Output, OutputStream, PortSpec,
        SpurCellMetadata,
    };

    const CELL_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
    const OTHER_CELL_ID: &str = "550e8400-e29b-41d4-a716-446655440001";

    fn code_cell(id: &str, source: &str, version: u64) -> Cell {
        Cell::Code(CodeCell {
            id: Some(id.to_string()),
            metadata: CellMetadata {
                spur: Some(SpurCellMetadata {
                    version,
                    last_edited_by: None,
                    datasource_setup: None,
                    dag: None,
                    code_type: None,
                    frontend: None,
                }),
                jute_deck: None,
                other: Map::new(),
            },
            source: MultilineString::Single(source.to_string()),
            execution_count: None,
            outputs: Vec::new(),
        })
    }

    fn notebook_with_source(source: &str) -> NotebookRoot {
        NotebookRoot {
            metadata: NotebookMetadata {
                kernelspec: None,
                language_info: None,
                orig_nbformat: None,
                title: None,
                authors: None,
                jute_deck: None,
                other: Map::new(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells: vec![code_cell(CELL_ID, source, 1)],
        }
    }

    fn notebook_with_two_code_cells() -> NotebookRoot {
        NotebookRoot {
            metadata: NotebookMetadata {
                kernelspec: None,
                language_info: None,
                orig_nbformat: None,
                title: None,
                authors: None,
                jute_deck: None,
                other: Map::new(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells: vec![
                code_cell(CELL_ID, "first", 1),
                code_cell(OTHER_CELL_ID, "second", 1),
            ],
        }
    }

    fn store_with_notebook() -> Arc<NotebookStore> {
        let store = NotebookStore::new(Arc::new(SaveCoordinator::default()));
        store.load("/tmp/test.ipynb", notebook_with_source("initial"));
        store
    }

    #[tokio::test]
    async fn concurrent_apply_does_not_deadlock() {
        let store = store_with_notebook();

        let handles = (0..16)
            .map(|index| {
                let store = Arc::clone(&store);
                tokio::task::spawn_blocking(move || {
                    store
                        .apply(NotebookOp::ApplyEdit {
                            id: CELL_ID.to_string(),
                            source: format!("value = {index}"),
                        })
                        .map(|delta| delta.version)
                })
            })
            .collect::<Vec<_>>();

        let versions = timeout(Duration::from_secs(2), async {
            join_all(handles)
                .await
                .into_iter()
                .map(|result| {
                    result
                        .expect("task should join")
                        .expect("apply should succeed")
                })
                .collect::<Vec<_>>()
        })
        .await
        .expect("concurrent apply should not deadlock");

        assert_eq!(versions.len(), 16);
    }

    #[test]
    fn version_monotonic() {
        let store = store_with_notebook();

        let write = store
            .apply(NotebookOp::WriteCell {
                id: CELL_ID.to_string(),
                source: "x = 1".to_string(),
                expected_version: Some(1),
                last_edited_by: Some("brain".to_string()),
            })
            .unwrap();
        let run = store
            .apply_run_event(CELL_ID, RunCellEvent::Stdout("hello".to_string()))
            .unwrap();

        let (_snapshot, version) = store.snapshot();
        assert_eq!(write.version, 2);
        assert_eq!(run.version, 3);
        assert_eq!(version, 3);
    }

    #[test]
    fn deltas_carry_owning_notebook_path() {
        let store = store_with_notebook(); // loads "/tmp/test.ipynb"

        let write = store
            .apply(NotebookOp::WriteCell {
                id: CELL_ID.to_string(),
                source: "x = 1".to_string(),
                expected_version: Some(1),
                last_edited_by: Some("brain".to_string()),
            })
            .unwrap();
        assert_eq!(write.path.as_deref(), Some("/tmp/test.ipynb"));

        let run = store
            .apply_run_event(CELL_ID, RunCellEvent::Stdout("hi".to_string()))
            .unwrap();
        assert_eq!(run.path.as_deref(), Some("/tmp/test.ipynb"));
    }

    #[test]
    fn delta_path_is_none_before_any_notebook_is_loaded() {
        let store = NotebookStore::new(Arc::new(SaveCoordinator::default()));
        let delta = store.publish_dag_status_changed(serde_json::json!({"nodes": []}));
        assert_eq!(delta.path, None);
    }

    #[tokio::test]
    async fn broadcast_fanout_reaches_multiple_receivers() {
        let store = store_with_notebook();
        let mut first = store.subscribe();
        let mut second = store.subscribe();

        let delta = store
            .apply(NotebookOp::ApplyEdit {
                id: CELL_ID.to_string(),
                source: "x = 2".to_string(),
            })
            .unwrap();

        let first_delta = first.recv().await.unwrap();
        let second_delta = second.recv().await.unwrap();

        assert_eq!(first_delta.version, delta.version);
        assert_eq!(second_delta.version, delta.version);
        assert!(matches!(
            first_delta.kind,
            DeltaKind::CellWritten { cell } if cell.id == CELL_ID
        ));
        assert!(matches!(
            second_delta.kind,
            DeltaKind::CellWritten { cell } if cell.id == CELL_ID
        ));
    }

    #[test]
    fn cell_written_delta_carries_post_mutation_cell() {
        let store = store_with_notebook();

        let delta = store
            .apply(NotebookOp::WriteCell {
                id: CELL_ID.to_string(),
                source: "answer = 42".to_string(),
                expected_version: Some(1),
                last_edited_by: Some("brain".to_string()),
            })
            .unwrap();

        let DeltaKind::CellWritten { cell } = delta.kind else {
            panic!("expected CellWritten delta");
        };
        assert_eq!(cell.id, CELL_ID);
        assert_eq!(cell.kind, "code");
        assert_eq!(cell.source, "answer = 42");
        // Two-phase build must reflect the post-bump version + metadata.
        assert_eq!(cell.version, delta.version);
        assert_eq!(cell.last_edited_by.as_deref(), Some("brain"));
    }

    #[test]
    fn cell_inserted_delta_carries_post_mutation_cell() {
        let store = store_with_notebook();

        let delta = store
            .apply(NotebookOp::InsertCell {
                kind: CellKind::Markdown,
                after_id: Some(CELL_ID.to_string()),
                source: "notes".to_string(),
                last_edited_by: Some("brain".to_string()),
                code_type: None,
            })
            .unwrap();

        let DeltaKind::CellInserted { cell, after_id } = delta.kind else {
            panic!("expected CellInserted delta");
        };
        assert_eq!(after_id.as_deref(), Some(CELL_ID));
        assert_eq!(cell.kind, "markdown");
        assert_eq!(cell.source, "notes");
        assert_eq!(cell.version, delta.version);
        assert_eq!(cell.last_edited_by.as_deref(), Some("brain"));
    }

    #[test]
    fn insert_cell_persists_initial_code_type_metadata() {
        let store = store_with_notebook();

        let delta = store
            .apply(NotebookOp::InsertCell {
                kind: CellKind::Code,
                after_id: Some(CELL_ID.to_string()),
                source: "console.log('hi')".to_string(),
                last_edited_by: Some("brain".to_string()),
                code_type: Some(CodeType::Javascript),
            })
            .expect("code cell insert succeeds");

        let DeltaKind::CellInserted { cell, .. } = delta.kind else {
            panic!("expected CellInserted delta");
        };
        assert_eq!(cell.kind, "code");
        assert_eq!(cell.code_type, Some(CodeType::Javascript));

        let (snapshot, _version) = store.snapshot();
        let Cell::Code(cell) = &snapshot.cells[1] else {
            panic!("expected inserted code cell");
        };
        let spur = cell.metadata.spur.as_ref().expect("spur metadata present");
        assert_eq!(spur.code_type, Some(CodeType::Javascript));
        assert_eq!(spur.version, delta.version);
    }

    #[test]
    fn loaded_delta_carries_root() {
        let store = NotebookStore::new(Arc::new(SaveCoordinator::default()));

        let delta = store.load("/tmp/test.ipynb", notebook_with_source("loaded body"));

        let DeltaKind::Loaded { root } = delta.kind else {
            panic!("expected Loaded delta");
        };
        assert_eq!(root.cells.len(), 1);
        let Cell::Code(cell) = &root.cells[0] else {
            panic!("expected code cell");
        };
        assert_eq!(
            cell.source,
            MultilineString::Single("loaded body".to_string())
        );
    }

    #[test]
    fn loaded_delta_carries_path_on_load_and_replace() {
        let store = NotebookStore::new(Arc::new(SaveCoordinator::default()));
        let load = store.load("/tmp/load.ipynb", notebook_with_source("a"));
        assert_eq!(load.path.as_deref(), Some("/tmp/load.ipynb"));
        assert!(matches!(load.kind, DeltaKind::Loaded { .. }));

        let replace = store.replace("/tmp/replace.ipynb", notebook_with_source("b"));
        assert_eq!(replace.path.as_deref(), Some("/tmp/replace.ipynb"));
        assert!(matches!(replace.kind, DeltaKind::Loaded { .. }));
    }

    #[tokio::test]
    async fn replace_marks_dirty_and_flushes_new_contents() {
        let dir = std::env::temp_dir().join(format!("jute-store-replace-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let initial_path = dir.join("initial.ipynb");
        let replacement_path = dir.join("replacement.ipynb");
        std::fs::write(
            &initial_path,
            serde_json::to_string_pretty(&notebook_with_source("initial")).unwrap(),
        )
        .unwrap();

        let store = NotebookStore::new(Arc::new(SaveCoordinator::default()));
        store.load(&initial_path, notebook_with_source("initial"));

        let delta = store.replace(&replacement_path, notebook_with_source("replacement"));

        let DeltaKind::Loaded { root } = delta.kind else {
            panic!("expected Loaded delta");
        };
        let Cell::Code(cell) = &root.cells[0] else {
            panic!("expected code cell");
        };
        assert_eq!(
            cell.source,
            MultilineString::Single("replacement".to_string())
        );
        assert!(store.is_dirty_for_test());
        assert_eq!(store.path().as_deref(), Some(replacement_path.as_path()));

        let (snapshot, _version) = store.snapshot();
        let Cell::Code(cell) = &snapshot.cells[0] else {
            panic!("expected code cell");
        };
        assert_eq!(
            cell.source,
            MultilineString::Single("replacement".to_string())
        );

        store.flush().await.unwrap();
        let contents = std::fs::read_to_string(&replacement_path).unwrap();
        let parsed: NotebookRoot = serde_json::from_str(&contents).unwrap();
        let Cell::Code(cell) = &parsed.cells[0] else {
            panic!("expected code cell");
        };
        assert_eq!(
            cell.source,
            MultilineString::Single("replacement".to_string())
        );

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn optimistic_concurrency_error_returned_on_stale_expected_version() {
        let store = store_with_notebook();

        store
            .apply(NotebookOp::WriteCell {
                id: CELL_ID.to_string(),
                source: "fresh".to_string(),
                expected_version: Some(1),
                last_edited_by: Some("brain".to_string()),
            })
            .unwrap();

        let error = store
            .apply(NotebookOp::WriteCell {
                id: CELL_ID.to_string(),
                source: "stale".to_string(),
                expected_version: Some(1),
                last_edited_by: Some("brain".to_string()),
            })
            .unwrap_err();

        assert_eq!(
            error,
            StoreError::OptimisticConcurrency {
                expected: 1,
                actual: 2,
            }
        );
    }

    #[test]
    fn check_cell_version_matches_apply_path() {
        let store = store_with_notebook();

        assert_eq!(store.check_cell_version(CELL_ID, 1), Ok(()));
        assert_eq!(
            store.check_cell_version(CELL_ID, 99),
            Err(StoreError::OptimisticConcurrency {
                expected: 99,
                actual: 1,
            })
        );
    }

    #[test]
    fn write_cell_expected_version_is_per_cell() {
        let store = NotebookStore::new(Arc::new(SaveCoordinator::default()));
        store.load("/tmp/test.ipynb", notebook_with_two_code_cells());

        store
            .apply(NotebookOp::WriteCell {
                id: CELL_ID.to_string(),
                source: "first updated".to_string(),
                expected_version: Some(1),
                last_edited_by: Some("brain".to_string()),
            })
            .unwrap();

        let delta = store
            .apply(NotebookOp::WriteCell {
                id: OTHER_CELL_ID.to_string(),
                source: "second updated".to_string(),
                expected_version: Some(1),
                last_edited_by: Some("brain".to_string()),
            })
            .unwrap();

        assert!(matches!(
            delta.kind,
            DeltaKind::CellWritten { cell } if cell.id == OTHER_CELL_ID
        ));
        let (snapshot, _version) = store.snapshot();
        let Cell::Code(cell) = &snapshot.cells[1] else {
            panic!("expected code cell");
        };
        assert_eq!(cell.metadata.spur.as_ref().unwrap().version, delta.version);
        assert_eq!(
            cell.source,
            MultilineString::Single("second updated".to_string())
        );
    }

    #[test]
    fn delete_cell_expected_version_is_per_cell() {
        let store = NotebookStore::new(Arc::new(SaveCoordinator::default()));
        store.load("/tmp/test.ipynb", notebook_with_two_code_cells());

        store
            .apply(NotebookOp::WriteCell {
                id: CELL_ID.to_string(),
                source: "first updated".to_string(),
                expected_version: Some(1),
                last_edited_by: Some("brain".to_string()),
            })
            .unwrap();

        let delta = store
            .apply(NotebookOp::DeleteCell {
                id: OTHER_CELL_ID.to_string(),
                expected_version: 1,
            })
            .unwrap();

        assert!(matches!(
            delta.kind,
            DeltaKind::CellDeleted { id } if id == OTHER_CELL_ID
        ));
        let (snapshot, _version) = store.snapshot();
        assert_eq!(snapshot.cells.len(), 1);
        assert_eq!(cell_id(&snapshot.cells[0]), Some(CELL_ID));
    }

    #[test]
    fn set_jute_deck_metadata_merges_patch() {
        let store = store_with_notebook();

        let patch = JuteDeckCellMetadata {
            layout: Some(JuteDeckLayout::Title),
            speaker_notes: Some("note 1".to_string()),
            ..Default::default()
        };
        store
            .apply(NotebookOp::SetJuteDeckMetadata {
                id: CELL_ID.to_string(),
                patch,
                expected_version: 1,
            })
            .expect("first patch applies");

        let patch = JuteDeckCellMetadata {
            layout: Some(JuteDeckLayout::Section),
            ..Default::default()
        };
        let delta = store
            .apply(NotebookOp::SetJuteDeckMetadata {
                id: CELL_ID.to_string(),
                patch,
                expected_version: 2,
            })
            .expect("second patch applies");

        assert!(matches!(
            delta.kind,
            DeltaKind::CellWritten { cell } if cell.id == CELL_ID
        ));
        let (snapshot, _version) = store.snapshot();
        let Cell::Code(cell) = &snapshot.cells[0] else {
            panic!("expected code cell");
        };
        let metadata = cell.metadata.jute_deck.as_ref().expect("metadata present");
        assert_eq!(metadata.layout, Some(JuteDeckLayout::Section));
        assert_eq!(metadata.speaker_notes.as_deref(), Some("note 1"));
        assert_eq!(cell.metadata.spur.as_ref().unwrap().version, delta.version);
        assert_eq!(
            cell.metadata
                .spur
                .as_ref()
                .unwrap()
                .last_edited_by
                .as_deref(),
            Some("brain")
        );
    }

    #[test]
    fn set_jute_deck_metadata_rejects_stale_version() {
        let store = store_with_notebook();

        let err = store
            .apply(NotebookOp::SetJuteDeckMetadata {
                id: CELL_ID.to_string(),
                patch: JuteDeckCellMetadata::default(),
                expected_version: 100,
            })
            .unwrap_err();

        assert_eq!(
            err,
            StoreError::OptimisticConcurrency {
                expected: 100,
                actual: 1,
            }
        );
    }

    #[test]
    fn set_spur_dag_metadata_sets_patch() {
        let store = store_with_notebook();

        let patch = CellDagMetadata {
            produces: vec![PortSpec {
                port: "sales".to_string(),
                repr: "dataframe".to_string(),
                display: Some("Sales".to_string()),
            }],
            consumes: vec!["config".to_string()],
            source: Some(DagSource {
                kind: "cell".to_string(),
                port: "raw".to_string(),
            }),
        };
        let delta = store
            .apply(NotebookOp::SetSpurDagMetadata {
                id: CELL_ID.to_string(),
                patch: patch.clone(),
                expected_version: 1,
            })
            .expect("dag metadata patch applies");

        assert!(matches!(
            delta.kind,
            DeltaKind::CellWritten { cell } if cell.id == CELL_ID
        ));
        let (snapshot, _version) = store.snapshot();
        let Cell::Code(cell) = &snapshot.cells[0] else {
            panic!("expected code cell");
        };
        let spur = cell.metadata.spur.as_ref().expect("spur metadata present");
        assert_eq!(spur.dag.as_ref(), Some(&patch));
        assert_eq!(spur.version, delta.version);
        assert_eq!(spur.last_edited_by.as_deref(), Some("brain"));
    }

    #[test]
    fn set_spur_dag_metadata_rejects_stale_version() {
        let store = store_with_notebook();

        let err = store
            .apply(NotebookOp::SetSpurDagMetadata {
                id: CELL_ID.to_string(),
                patch: CellDagMetadata::default(),
                expected_version: 100,
            })
            .unwrap_err();

        assert_eq!(
            err,
            StoreError::OptimisticConcurrency {
                expected: 100,
                actual: 1,
            }
        );
    }

    #[test]
    fn set_spur_code_type_metadata_sets_patch() {
        let store = store_with_notebook();

        let delta = store
            .apply(NotebookOp::SetSpurCodeTypeMetadata {
                id: CELL_ID.to_string(),
                code_type: CodeType::Rust,
                expected_version: 1,
            })
            .expect("code_type metadata patch applies");

        assert!(matches!(
            delta.kind,
            DeltaKind::CellWritten { cell } if cell.id == CELL_ID
                && cell.code_type == Some(CodeType::Rust)
        ));
        let (snapshot, _version) = store.snapshot();
        let Cell::Code(cell) = &snapshot.cells[0] else {
            panic!("expected code cell");
        };
        let spur = cell.metadata.spur.as_ref().expect("spur metadata present");
        assert_eq!(spur.code_type, Some(CodeType::Rust));
        assert_eq!(spur.version, delta.version);
        assert_eq!(spur.last_edited_by.as_deref(), Some("brain"));
    }

    #[test]
    fn set_spur_code_type_metadata_rejects_stale_version() {
        let store = store_with_notebook();

        let err = store
            .apply(NotebookOp::SetSpurCodeTypeMetadata {
                id: CELL_ID.to_string(),
                code_type: CodeType::Python,
                expected_version: 100,
            })
            .unwrap_err();

        assert_eq!(
            err,
            StoreError::OptimisticConcurrency {
                expected: 100,
                actual: 1,
            }
        );
    }

    #[test]
    fn spur_dag_metadata_serializes_empty_vectors_and_round_trips() {
        let metadata = CellDagMetadata {
            produces: Vec::new(),
            consumes: vec!["blast".to_string(), "cochange".to_string()],
            source: None,
        };

        let value = serde_json::to_value(&metadata).expect("dag metadata serializes");
        assert_eq!(
            value,
            serde_json::json!({
                "produces": [],
                "consumes": ["blast", "cochange"]
            })
        );

        let round_tripped: CellDagMetadata =
            serde_json::from_value(value).expect("dag metadata deserializes");
        assert_eq!(round_tripped, metadata);
    }

    #[test]
    fn spur_dag_metadata_deserializes_missing_produces_as_empty_vec() {
        let metadata: CellDagMetadata = serde_json::from_value(serde_json::json!({
            "consumes": ["blast", "cochange"]
        }))
        .expect("legacy dag metadata deserializes");

        assert!(metadata.produces.is_empty());
        assert_eq!(
            metadata.consumes,
            vec!["blast".to_string(), "cochange".to_string()]
        );
        assert_eq!(metadata.source, None);
    }

    #[test]
    fn spur_dag_metadata_survives_later_cell_write() {
        let store = store_with_notebook();

        let patch = CellDagMetadata {
            produces: vec![PortSpec {
                port: "sales".to_string(),
                repr: "dataframe".to_string(),
                display: None,
            }],
            ..Default::default()
        };
        store
            .apply(NotebookOp::SetSpurDagMetadata {
                id: CELL_ID.to_string(),
                patch: patch.clone(),
                expected_version: 1,
            })
            .expect("dag metadata patch applies");

        store
            .apply(NotebookOp::WriteCell {
                id: CELL_ID.to_string(),
                source: "updated".to_string(),
                expected_version: Some(2),
                last_edited_by: Some("brain".to_string()),
            })
            .expect("write applies");

        let (snapshot, _version) = store.snapshot();
        let Cell::Code(cell) = &snapshot.cells[0] else {
            panic!("expected code cell");
        };
        assert_eq!(
            cell.metadata.spur.as_ref().unwrap().dag.as_ref(),
            Some(&patch)
        );
    }

    #[test]
    fn run_event_appends_output() {
        let store = store_with_notebook();

        store
            .apply_run_event(CELL_ID, RunCellEvent::Stdout("hello".to_string()))
            .unwrap();

        let (snapshot, _version) = store.snapshot();
        let Cell::Code(cell) = &snapshot.cells[0] else {
            panic!("expected code cell");
        };
        assert_eq!(
            cell.outputs,
            vec![Output::Stream(OutputStream {
                name: "stdout".to_string(),
                text: MultilineString::Single("hello".to_string()),
                other: Map::new(),
            })]
        );
    }

    #[tokio::test]
    async fn autosave_persists_dirty_store_after_debounce_without_explicit_flush() {
        let dir = std::env::temp_dir().join(format!("jute-store-autosave-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notebook.ipynb");
        std::fs::write(
            &path,
            serde_json::to_string_pretty(&notebook_with_source("initial")).unwrap(),
        )
        .unwrap();

        let store = NotebookStore::new(Arc::new(SaveCoordinator::default()));
        store.load(&path, notebook_with_source("initial"));
        store
            .apply(NotebookOp::WriteCell {
                id: CELL_ID.to_string(),
                source: "autosaved".to_string(),
                expected_version: Some(1),
                last_edited_by: Some("brain".to_string()),
            })
            .unwrap();

        tokio::time::sleep(Duration::from_millis(1_100)).await;

        let contents = std::fs::read_to_string(&path).unwrap();
        let parsed: NotebookRoot = serde_json::from_str(&contents).unwrap();
        let Cell::Code(cell) = &parsed.cells[0] else {
            panic!("expected code cell");
        };
        assert_eq!(
            cell.source,
            MultilineString::Single("autosaved".to_string())
        );

        std::fs::remove_dir_all(dir).unwrap();
    }
}
