//! Defines state and stores for the Tauri application.

use std::{
    env,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};

use dashmap::DashMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{
    backend::local::LocalKernel,
    backend::notebook::{kernelspec_for, CodeType, NotebookMetadata, NotebookRoot},
    commands::{DatasourceEntry, SaveCoordinator},
    notebook_store::{merge_authoritative_spur_metadata_for_save, NotebookStore},
};

/// Current schema version for notebook datasource catalog entries.
pub const DATASOURCE_CATALOG_SCHEMA_VERSION: u32 = 1;
const SPUR_METADATA_KEY: &str = "spur";
const DATASOURCES_METADATA_KEY: &str = "datasources";

/// In-daemon events produced by notebook state mutations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonEvent {
    /// The active notebook's datasource catalog changed.
    DatasourcesChanged(Vec<DatasourceEntry>),
}

/// In-memory catalog of datasources attached to the active notebook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasourceCatalog {
    /// Catalog schema version.
    pub schema_version: u32,
    /// Attached datasource entries.
    pub entries: Vec<DatasourceEntry>,
}

impl Default for DatasourceCatalog {
    fn default() -> Self {
        Self {
            schema_version: DATASOURCE_CATALOG_SCHEMA_VERSION,
            entries: Vec::new(),
        }
    }
}

impl DatasourceCatalog {
    /// Attach or replace a datasource entry by name.
    pub fn attach(&mut self, entry: DatasourceEntry) {
        if let Some(existing) = self
            .entries
            .iter_mut()
            .find(|existing| existing.name == entry.name)
        {
            *existing = entry;
        } else {
            self.entries.push(entry);
        }
    }

    /// Detach a datasource by name, returning the removed entry when present.
    pub fn detach(&mut self, name: &str) -> Option<DatasourceEntry> {
        let index = self.entries.iter().position(|entry| entry.name == name)?;
        Some(self.entries.remove(index))
    }

    /// List attached datasources in catalog order.
    pub fn list(&self) -> Vec<DatasourceEntry> {
        self.entries.clone()
    }

    /// Serialize this catalog into notebook metadata under
    /// `metadata.spur.datasources`.
    pub fn persist_to_metadata(
        &self,
        metadata: &mut NotebookMetadata,
        _notebook_path: Option<&Path>,
    ) {
        if self.entries.is_empty() && !metadata_has_datasources(metadata) {
            return;
        }

        let workspace_root = workspace_root();
        let entries = self
            .entries
            .iter()
            .map(|entry| {
                let mut entry = entry.clone();
                let absolute_path =
                    normalize_path_for_storage(Path::new(&entry.path), &workspace_root);
                entry.path = path_to_string(&absolute_path);
                StoredDatasourceEntry {
                    workspace_relative_path: workspace_relative_path(
                        &absolute_path,
                        &workspace_root,
                    ),
                    entry,
                }
            })
            .collect();
        let persisted = StoredDatasourceCatalog {
            schema_version: self.schema_version,
            entries,
        };

        let value =
            serde_json::to_value(persisted).expect("datasource catalog metadata serializes");
        let spur = metadata
            .other
            .entry(SPUR_METADATA_KEY.to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        if !spur.is_object() {
            *spur = Value::Object(Map::new());
        }
        spur.as_object_mut()
            .expect("spur metadata is an object")
            .insert(DATASOURCES_METADATA_KEY.to_owned(), value);
    }

    /// Hydrate the in-memory catalog from `metadata.spur.datasources`.
    ///
    /// Missing, malformed, or unsupported versions intentionally produce an
    /// empty catalog so older notebooks continue to load.
    pub fn hydrate_from_metadata(
        metadata: &NotebookMetadata,
        _notebook_path: Option<&Path>,
    ) -> Self {
        let Some(value) = metadata
            .other
            .get(SPUR_METADATA_KEY)
            .and_then(Value::as_object)
            .and_then(|spur| spur.get(DATASOURCES_METADATA_KEY))
        else {
            return Self::default();
        };

        let Ok(persisted) = serde_json::from_value::<StoredDatasourceCatalog>(value.clone()) else {
            return Self::default();
        };
        if persisted.schema_version != DATASOURCE_CATALOG_SCHEMA_VERSION {
            return Self::default();
        }

        let workspace_root = workspace_root();
        let entries = persisted
            .entries
            .into_iter()
            .map(|stored| {
                let mut entry = stored.entry;
                entry.path = resolve_stored_path(
                    &entry.path,
                    stored.workspace_relative_path.as_deref(),
                    &workspace_root,
                );
                entry
            })
            .collect();

        Self {
            schema_version: DATASOURCE_CATALOG_SCHEMA_VERSION,
            entries,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredDatasourceCatalog {
    schema_version: u32,
    entries: Vec<StoredDatasourceEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredDatasourceEntry {
    #[serde(flatten)]
    entry: DatasourceEntry,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace_relative_path: Option<String>,
}

fn metadata_has_datasources(metadata: &NotebookMetadata) -> bool {
    metadata
        .other
        .get(SPUR_METADATA_KEY)
        .and_then(Value::as_object)
        .is_some_and(|spur| spur.contains_key(DATASOURCES_METADATA_KEY))
}

fn workspace_root() -> PathBuf {
    env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn normalize_path_for_storage(path: &Path, workspace_root: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workspace_root.join(path)
    };
    std::fs::canonicalize(&absolute).unwrap_or_else(|_| lexical_normalize(&absolute))
}

fn workspace_relative_path(path: &Path, workspace_root: &Path) -> Option<String> {
    let root = normalize_path_for_storage(workspace_root, workspace_root);
    path.strip_prefix(&root)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .map(path_to_string)
}

fn resolve_stored_path(
    path: &str,
    workspace_relative_path: Option<&str>,
    workspace_root: &Path,
) -> String {
    let absolute = normalize_path_for_storage(Path::new(path), workspace_root);
    let Some(relative) = workspace_relative_path else {
        return path_to_string(&absolute);
    };

    let relative_absolute = normalize_path_for_storage(Path::new(relative), workspace_root);
    if relative_absolute.exists() || !absolute.exists() {
        path_to_string(&relative_absolute)
    } else {
        path_to_string(&absolute)
    }
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn path_to_string(path: &Path) -> String {
    path.display().to_string()
}

/// Stable prefix used for notebook path-derived kernel slots.
pub(crate) const NOTEBOOK_SLOT_PREFIX: &str = "notebook:";

/// Derive the stable in-memory kernel slot ID for a notebook path.
pub fn notebook_slot_id(path: &str) -> String {
    format!("{NOTEBOOK_SLOT_PREFIX}{path}")
}

/// Derive the per-notebook kernel slot ID for a per-cell code type.
pub fn slot_id_for(path: &str, code_type: CodeType) -> String {
    slot_id_for_spec(path, kernelspec_for(code_type))
}

/// Derive the per-notebook kernel slot ID for a kernelspec name.
pub fn slot_id_for_spec(path: &str, spec_name: &str) -> String {
    format!("{}#{spec_name}", notebook_slot_id(path))
}

/// Derive the fallback kernel slot ID for windows without a notebook path.
pub(crate) fn window_slot_id(label: &str) -> String {
    format!("window:{label}")
}

/// Stable kernel slot for a notebook.
pub struct KernelSlot {
    pub(crate) kernel: Option<LocalKernel>,
    generation: AtomicU64,
    spec_name: String,
}

impl KernelSlot {
    /// Create an empty slot for the given kernel spec.
    pub fn new(spec_name: String) -> Self {
        Self {
            kernel: None,
            generation: AtomicU64::new(0),
            spec_name,
        }
    }

    pub(crate) fn with_kernel(kernel: LocalKernel, spec_name: String) -> Self {
        let mut slot = Self::new(spec_name.clone());
        slot.kernel = Some(kernel);
        slot.record_start(spec_name);
        slot
    }

    /// Return the current in-memory slot generation.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    /// Return the kernel spec name used by the latest successful start.
    pub fn spec_name(&self) -> &str {
        &self.spec_name
    }

    pub(crate) fn replace_kernel(&mut self, kernel: LocalKernel, spec_name: String) -> u64 {
        self.kernel = Some(kernel);
        self.record_start(spec_name)
    }

    pub(crate) fn record_start(&mut self, spec_name: String) -> u64 {
        self.spec_name = spec_name;
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }
}

/// State for the running Tauri application.
pub struct State {
    /// Current kernel slots in the application, keyed by stable slot ID.
    pub kernels: DashMap<String, KernelSlot>,

    /// Coordinator for debounced notebook saves.
    pub save_coordinator: SaveCoordinator,

    /// In-memory datasource catalog for the active notebook.
    pub datasource_catalog: Arc<Mutex<DatasourceCatalog>>,

    /// In-process daemon event fan-out for subscribers.
    pub event_tx: tokio::sync::broadcast::Sender<DaemonEvent>,

    /// Lazily initialized authoritative notebook document store.
    notebook: Arc<Mutex<Option<Arc<NotebookStore>>>>,
}

impl Default for State {
    fn default() -> Self {
        let datasource_catalog = Arc::new(Mutex::new(DatasourceCatalog::default()));
        let catalog_for_save = Arc::clone(&datasource_catalog);
        let notebook = Arc::new(Mutex::new(None::<Arc<NotebookStore>>));
        let notebook_for_save = Arc::clone(&notebook);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        Self {
            kernels: DashMap::new(),
            save_coordinator: SaveCoordinator::with_before_save(
                move |path, contents: &mut NotebookRoot| {
                    catalog_for_save
                        .lock()
                        .persist_to_metadata(&mut contents.metadata, Some(path));
                    let Some(store) = notebook_for_save.lock().as_ref().cloned() else {
                        return;
                    };
                    if store.path().as_deref() != Some(path) {
                        return;
                    }
                    let (authoritative, _version) = store.snapshot();
                    merge_authoritative_spur_metadata_for_save(contents, &authoritative);
                },
            ),
            datasource_catalog,
            event_tx,
            notebook,
        }
    }
}

impl State {
    /// Create a new state object.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach or replace a datasource entry, then notify daemon event subscribers.
    pub fn attach_datasource(&self, entry: DatasourceEntry) {
        let entries = {
            let mut catalog = self.datasource_catalog.lock();
            catalog.attach(entry);
            catalog.list()
        };
        self.emit_datasources_changed(entries);
    }

    /// Attach or replace multiple datasource entries, then notify subscribers once.
    pub fn attach_datasources(&self, entries: Vec<DatasourceEntry>) {
        if entries.is_empty() {
            return;
        }
        let entries = {
            let mut catalog = self.datasource_catalog.lock();
            for entry in entries {
                catalog.attach(entry);
            }
            catalog.list()
        };
        self.emit_datasources_changed(entries);
    }

    /// Detach a datasource entry, notifying subscribers only when the catalog changes.
    pub fn detach_datasource(&self, name: &str) -> Option<DatasourceEntry> {
        let (removed, entries) = {
            let mut catalog = self.datasource_catalog.lock();
            let removed = catalog.detach(name);
            let entries = removed.as_ref().map(|_| catalog.list());
            (removed, entries)
        };
        if let Some(entries) = entries {
            self.emit_datasources_changed(entries);
        }
        removed
    }

    /// Return the process-wide notebook store, initializing it on first use.
    pub fn get_notebook(&self) -> Arc<NotebookStore> {
        let mut notebook = self.notebook.lock();
        notebook
            .get_or_insert_with(|| NotebookStore::new(Arc::new(self.save_coordinator.clone())))
            .clone()
    }

    pub(crate) fn emit_datasources_changed(&self, entries: Vec<DatasourceEntry>) {
        let _ = self.event_tx.send(DaemonEvent::DatasourcesChanged(entries));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kernel_slot_generation_starts_at_one_and_increments_with_stable_slot_id() {
        let path = "/tmp/notebooks/demo.ipynb";
        let slot_id = notebook_slot_id(path);
        let state = State::new();
        state
            .kernels
            .insert(slot_id.clone(), KernelSlot::new("python3".to_string()));

        {
            let mut slot = state.kernels.get_mut(&slot_id).unwrap();
            assert_eq!(slot.record_start("python3".to_string()), 1);
            assert_eq!(slot.generation(), 1);
        }

        let restart_slot_id = notebook_slot_id(path);
        assert_eq!(restart_slot_id, slot_id);

        {
            let mut slot = state.kernels.get_mut(&restart_slot_id).unwrap();
            assert_eq!(slot.record_start("python3".to_string()), 2);
            assert_eq!(slot.generation(), 2);
            assert_eq!(slot.spec_name(), "python3");
        }

        {
            let mut slot = state.kernels.get_mut(&restart_slot_id).unwrap();
            assert_eq!(slot.record_start("python3-debug".to_string()), 3);
            assert_eq!(slot.generation(), 3);
            assert_eq!(slot.spec_name(), "python3-debug");
        }
    }

    #[test]
    fn kernel_slot_generation_resets_to_one_after_daemon_state_restart() {
        let path = "/tmp/notebooks/restart.ipynb";
        let slot_id = notebook_slot_id(path);
        let state = State::new();
        state
            .kernels
            .insert(slot_id.clone(), KernelSlot::new("python3".to_string()));

        {
            let mut slot = state.kernels.get_mut(&slot_id).unwrap();
            assert_eq!(slot.record_start("python3".to_string()), 1);
            assert_eq!(slot.record_start("python3".to_string()), 2);
        }

        let restarted_state = State::new();
        restarted_state
            .kernels
            .insert(slot_id.clone(), KernelSlot::new("python3".to_string()));
        let mut restarted_slot = restarted_state.kernels.get_mut(&slot_id).unwrap();

        assert_eq!(restarted_slot.record_start("python3".to_string()), 1);
        assert_eq!(restarted_slot.generation(), 1);
    }

    #[test]
    fn notebook_store_is_initialized_lazily_and_reused() {
        let state = State::new();

        let first = state.get_notebook();
        let second = state.get_notebook();

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn catalog_loads_pre_tables_entries() {
        let metadata: NotebookMetadata = serde_json::from_value(json!({
            "spur": {
                "datasources": {
                    "schema_version": 1,
                    "entries": [
                        {
                            "name": "sales",
                            "path": "/tmp/sales.csv",
                            "kind": "csv",
                            "group": null,
                            "columns": [
                                {
                                    "name": "region",
                                    "sqlType": "VARCHAR"
                                }
                            ],
                            "rowCount": 2
                        }
                    ]
                }
            }
        }))
        .expect("legacy metadata decodes");

        let catalog = DatasourceCatalog::hydrate_from_metadata(&metadata, None);

        assert_eq!(catalog.schema_version, DATASOURCE_CATALOG_SCHEMA_VERSION);
        assert_eq!(catalog.entries.len(), 1);
        assert_eq!(catalog.entries[0].name, "sales");
        assert!(catalog.entries[0].tables.is_empty());
    }
}
