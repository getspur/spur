//! Authoritative in-memory notebook document store.

use std::{
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};

use parking_lot::{Mutex, RwLock};
use serde_json::Map;
use thiserror::Error;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::{
    backend::{
        commands::RunCellEvent,
        notebook::{
            Cell, CellMetadata, CodeCell, MarkdownCell, MultilineString, NotebookMetadata,
            NotebookRoot, Output, OutputDisplayData, OutputError, OutputExecuteResult,
            OutputStream, RawCell,
        },
    },
    commands::SaveCoordinator,
    Error as JuteError,
};

/// Authoritative Rust-owned notebook document store.
pub struct NotebookStore {
    inner: Arc<RwLock<NotebookRoot>>,
    version: AtomicU64,
    dirty: AtomicBool,
    broadcast: broadcast::Sender<NotebookDelta>,
    save_coord: Arc<SaveCoordinator>,
    path: Mutex<Option<PathBuf>>,
}

/// Cell kind to create when inserting a notebook cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Replace the source of an existing cell, optionally checking document version.
    WriteCell {
        /// Cell identifier.
        id: String,
        /// New cell source.
        source: String,
        /// Expected store version for optimistic concurrency.
        expected_version: Option<u64>,
    },
    /// Insert a new cell after an optional existing cell.
    InsertCell {
        /// Kind of cell to insert.
        kind: CellKind,
        /// Existing cell identifier to insert after.
        after_id: Option<String>,
        /// Initial cell source.
        source: String,
    },
    /// Delete an existing cell after checking document version.
    DeleteCell {
        /// Cell identifier.
        id: String,
        /// Expected store version for optimistic concurrency.
        expected_version: u64,
    },
    /// Replace the source of an existing cell without concurrency checks.
    ApplyEdit {
        /// Cell identifier.
        id: String,
        /// New cell source.
        source: String,
    },
}

/// Broadcast notification emitted after each store mutation.
#[derive(Debug, Clone)]
pub struct NotebookDelta {
    /// Monotonic document version after the mutation.
    pub version: u64,
    /// Kind of mutation represented by this delta.
    pub kind: DeltaKind,
}

/// Kind of store mutation represented by a [`NotebookDelta`].
#[derive(Debug, Clone)]
pub enum DeltaKind {
    /// A cell's source was replaced.
    CellWritten {
        /// Cell identifier.
        id: String,
    },
    /// A new cell was inserted.
    CellInserted {
        /// New cell identifier.
        id: String,
        /// Kind of inserted cell.
        kind: CellKind,
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
        event: RunCellEvent,
    },
    /// A notebook was loaded into the store.
    Loaded,
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
        Arc::new(Self {
            inner: Arc::new(RwLock::new(empty_notebook())),
            version: AtomicU64::new(0),
            dirty: AtomicBool::new(false),
            broadcast,
            save_coord,
            path: Mutex::new(None),
        })
    }

    /// Load a notebook document into the store.
    pub fn load<P>(&self, path: P, root: NotebookRoot) -> NotebookDelta
    where
        P: Into<PathBuf>,
    {
        let version = {
            let mut stored_path = self.path.lock();
            let mut inner = self.inner.write();
            *inner = root;
            *stored_path = Some(path.into());
            self.dirty.store(false, Ordering::SeqCst);
            self.bump_version()
        };

        let delta = NotebookDelta {
            version,
            kind: DeltaKind::Loaded,
        };
        self.publish(&delta);
        delta
    }

    /// Return a point-in-time notebook snapshot and version.
    pub fn snapshot(&self) -> (NotebookRoot, u64) {
        let inner = self.inner.read();
        let version = self.version.load(Ordering::SeqCst);
        (inner.clone(), version)
    }

    /// Apply a notebook edit operation.
    pub fn apply(&self, op: NotebookOp) -> Result<NotebookDelta, StoreError> {
        let mut root = self.inner.write();
        let kind = match op {
            NotebookOp::WriteCell {
                id,
                source,
                expected_version,
            } => {
                if let Some(expected) = expected_version {
                    self.ensure_version(expected)?;
                }
                let cell = find_cell_mut(&mut root, &id)
                    .ok_or_else(|| StoreError::CellNotFound { id: id.clone() })?;
                set_cell_source(cell, source);
                DeltaKind::CellWritten { id }
            }
            NotebookOp::InsertCell {
                kind,
                after_id,
                source,
            } => {
                let insert_at = match after_id.as_deref() {
                    Some(after_id) => find_cell_index(&root, after_id)
                        .map(|index| index + 1)
                        .ok_or_else(|| StoreError::CellNotFound {
                            id: after_id.to_string(),
                        })?,
                    None => root.cells.len(),
                };
                let id = Uuid::new_v4().to_string();
                root.cells
                    .insert(insert_at, make_cell(kind, id.clone(), source));
                DeltaKind::CellInserted { id, kind, after_id }
            }
            NotebookOp::DeleteCell {
                id,
                expected_version,
            } => {
                self.ensure_version(expected_version)?;
                let index = find_cell_index(&root, &id)
                    .ok_or_else(|| StoreError::CellNotFound { id: id.clone() })?;
                root.cells.remove(index);
                DeltaKind::CellDeleted { id }
            }
            NotebookOp::ApplyEdit { id, source } => {
                let cell = find_cell_mut(&mut root, &id)
                    .ok_or_else(|| StoreError::CellNotFound { id: id.clone() })?;
                set_cell_source(cell, source);
                DeltaKind::CellWritten { id }
            }
        };

        let delta = NotebookDelta {
            version: self.bump_version(),
            kind,
        };
        self.dirty.store(true, Ordering::SeqCst);
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

        let delta = NotebookDelta {
            version: self.bump_version(),
            kind: DeltaKind::RunCellEvent { cell_id, event },
        };
        self.dirty.store(true, Ordering::SeqCst);
        drop(root);
        self.publish(&delta);
        Ok(delta)
    }

    /// Flush the current notebook contents to disk through the save coordinator.
    pub async fn flush(&self) -> Result<(), io::Error> {
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

    fn ensure_version(&self, expected: u64) -> Result<(), StoreError> {
        let actual = self.version.load(Ordering::SeqCst);
        if expected == actual {
            Ok(())
        } else {
            Err(StoreError::OptimisticConcurrency { expected, actual })
        }
    }

    fn bump_version(&self) -> u64 {
        self.version.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn publish(&self, delta: &NotebookDelta) {
        let _ = self.broadcast.send(delta.clone());
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
            other: Map::new(),
        },
        nbformat_minor: 5,
        nbformat: 4,
        cells: Vec::new(),
    }
}

fn make_cell(kind: CellKind, id: String, source: String) -> Cell {
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
            metadata: empty_cell_metadata(),
            source: MultilineString::Single(source),
            execution_count: None,
            outputs: Vec::new(),
        }),
    }
}

fn empty_cell_metadata() -> CellMetadata {
    CellMetadata {
        spur: None,
        other: Map::new(),
    }
}

fn find_cell_index(root: &NotebookRoot, id: &str) -> Option<usize> {
    root.cells
        .iter()
        .position(|cell| cell_id(cell).is_some_and(|cell_id| cell_id == id))
}

fn find_cell_mut<'a>(root: &'a mut NotebookRoot, id: &str) -> Option<&'a mut Cell> {
    root.cells
        .iter_mut()
        .find(|cell| cell_id(cell).is_some_and(|cell_id| cell_id == id))
}

fn cell_id(cell: &Cell) -> Option<&str> {
    match cell {
        Cell::Raw(cell) => cell.id.as_deref(),
        Cell::Markdown(cell) => cell.id.as_deref(),
        Cell::Code(cell) => cell.id.as_deref(),
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
                ename: "KernelDisconnect".to_string(),
                evalue: message.clone(),
                traceback: Vec::new(),
                other: Map::new(),
            }));
        }
        RunCellEvent::Finished { exec_count, .. } => {
            cell.execution_count = *exec_count;
        }
    }
}

fn stream_output(name: &str, text: &str) -> Output {
    Output::Stream(OutputStream {
        name: name.to_string(),
        text: MultilineString::Single(text.to_string()),
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
        Cell, CellMetadata, CodeCell, MultilineString, NotebookMetadata, Output, OutputStream,
    };

    const CELL_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

    fn notebook_with_source(source: &str) -> NotebookRoot {
        NotebookRoot {
            metadata: NotebookMetadata {
                kernelspec: None,
                language_info: None,
                orig_nbformat: None,
                title: None,
                authors: None,
                other: Map::new(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells: vec![Cell::Code(CodeCell {
                id: Some(CELL_ID.to_string()),
                metadata: CellMetadata {
                    spur: None,
                    other: Map::new(),
                },
                source: MultilineString::Single(source.to_string()),
                execution_count: None,
                outputs: Vec::new(),
            })],
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
            DeltaKind::CellWritten { id } if id == CELL_ID
        ));
        assert!(matches!(
            second_delta.kind,
            DeltaKind::CellWritten { id } if id == CELL_ID
        ));
    }

    #[test]
    fn optimistic_concurrency_error_returned_on_stale_expected_version() {
        let store = store_with_notebook();

        store
            .apply(NotebookOp::WriteCell {
                id: CELL_ID.to_string(),
                source: "fresh".to_string(),
                expected_version: Some(1),
            })
            .unwrap();

        let error = store
            .apply(NotebookOp::WriteCell {
                id: CELL_ID.to_string(),
                source: "stale".to_string(),
                expected_version: Some(1),
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
}
