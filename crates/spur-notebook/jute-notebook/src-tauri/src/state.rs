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
    identity::NotebookId,
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
    NotebookId::for_saved_path(path).kernel_slot_id()
}

/// Derive the per-notebook kernel slot ID for a per-cell code type.
pub fn slot_id_for(path: &str, code_type: CodeType) -> String {
    slot_id_for_spec(path, kernelspec_for(code_type))
}

/// Derive the per-notebook kernel slot ID for a kernelspec name.
pub fn slot_id_for_spec(path: &str, spec_name: &str) -> String {
    NotebookId::for_saved_path(path).kernel_slot_id_for_spec(spec_name)
}

/// Recover the notebook path from a notebook-derived kernel slot ID.
pub fn notebook_path_from_slot_id<'a>(slot_id: &'a str, spec_name: &str) -> Option<&'a str> {
    let path = slot_id.strip_prefix(NOTEBOOK_SLOT_PREFIX)?;
    let spec_suffix = format!("#{spec_name}");
    let path = path.strip_suffix(spec_suffix.as_str()).unwrap_or(path);
    (!path.is_empty()).then_some(path)
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

    /// Owning kernel slot for each open frontend comm, keyed by comm ID.
    comm_owner: DashMap<String, String>,

    /// Coordinator for debounced notebook saves.
    pub save_coordinator: SaveCoordinator,

    /// Compatibility mirror for the focused notebook's datasource catalog.
    ///
    /// New daemon-layer code should use the keyed catalog helpers below. This
    /// mirror keeps older call sites on focused single-window semantics until
    /// their protocols carry explicit notebook targets.
    pub datasource_catalog: Arc<Mutex<DatasourceCatalog>>,

    /// Authoritative notebook document stores keyed by stable notebook ID.
    pub notebooks: Arc<DashMap<NotebookId, Arc<NotebookStore>>>,

    /// In-memory datasource catalogs keyed by stable notebook ID.
    pub datasource_catalogs: Arc<DashMap<NotebookId, Arc<Mutex<DatasourceCatalog>>>>,

    /// Focused notebook ID used by implicit daemon operations.
    focused: Mutex<Option<NotebookId>>,

    /// In-process daemon event fan-out for subscribers.
    pub event_tx: tokio::sync::broadcast::Sender<DaemonEvent>,
}

impl Default for State {
    fn default() -> Self {
        let datasource_catalog = Arc::new(Mutex::new(DatasourceCatalog::default()));
        let notebooks = Arc::new(DashMap::<NotebookId, Arc<NotebookStore>>::new());
        let datasource_catalogs =
            Arc::new(DashMap::<NotebookId, Arc<Mutex<DatasourceCatalog>>>::new());
        let notebooks_for_save = Arc::clone(&notebooks);
        let catalogs_for_save = Arc::clone(&datasource_catalogs);
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        Self {
            kernels: DashMap::new(),
            comm_owner: DashMap::new(),
            save_coordinator: SaveCoordinator::with_before_save(
                move |path, contents: &mut NotebookRoot| {
                    let path_id = NotebookId::for_saved_path(path);
                    let target = notebooks_for_save
                        .get(&path_id)
                        .map(|entry| (entry.key().clone(), Arc::clone(entry.value())))
                        .or_else(|| {
                            notebooks_for_save.iter().find_map(|entry| {
                                (entry.value().path().as_deref() == Some(path))
                                    .then(|| (entry.key().clone(), Arc::clone(entry.value())))
                            })
                        });
                    let Some((target_id, store)) = target else {
                        return;
                    };
                    if let Some(catalog) = catalogs_for_save.get(&target_id) {
                        catalog
                            .lock()
                            .persist_to_metadata(&mut contents.metadata, Some(path));
                    }
                    let (authoritative, _version) = store.snapshot();
                    merge_authoritative_spur_metadata_for_save(contents, &authoritative);
                },
            ),
            datasource_catalog,
            notebooks,
            datasource_catalogs,
            focused: Mutex::new(None),
            event_tx,
        }
    }
}

pub(crate) fn record_comm_open(state: &State, slot_id: &str, comm_id: &str) {
    state
        .comm_owner
        .insert(comm_id.to_owned(), slot_id.to_owned());
}

pub(crate) fn remove_comm_owner(state: &State, comm_id: &str) {
    state.comm_owner.remove(comm_id);
}

pub(crate) fn clear_comm_owners_for_slot(state: &State, slot_id: &str) {
    state.comm_owner.retain(|_, owner| owner != slot_id);
}

/// Return the kernel slot that owns an open comm ID.
pub fn slot_for_comm(state: &State, comm_id: &str) -> Option<String> {
    state
        .comm_owner
        .get(comm_id)
        .map(|owner| owner.value().clone())
}

impl State {
    /// Create a new state object.
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach or replace a datasource entry, then notify daemon event subscribers.
    pub fn attach_datasource(&self, entry: DatasourceEntry) {
        let entries = {
            let catalog = self.focused_datasource_catalog();
            let mut catalog = catalog.lock();
            catalog.attach(entry);
            catalog.list()
        };
        self.sync_focused_catalog_mirror();
        self.emit_datasources_changed(entries);
    }

    /// Attach or replace multiple datasource entries, then notify subscribers once.
    pub fn attach_datasources(&self, entries: Vec<DatasourceEntry>) {
        if entries.is_empty() {
            return;
        }
        let entries = {
            let catalog = self.focused_datasource_catalog();
            let mut catalog = catalog.lock();
            for entry in entries {
                catalog.attach(entry);
            }
            catalog.list()
        };
        self.sync_focused_catalog_mirror();
        self.emit_datasources_changed(entries);
    }

    /// Detach a datasource entry, notifying subscribers only when the catalog changes.
    pub fn detach_datasource(&self, name: &str) -> Option<DatasourceEntry> {
        let (removed, entries) = {
            let catalog = self.focused_datasource_catalog();
            let mut catalog = catalog.lock();
            let removed = catalog.detach(name);
            let entries = removed.as_ref().map(|_| catalog.list());
            (removed, entries)
        };
        if let Some(entries) = entries {
            self.sync_focused_catalog_mirror();
            self.emit_datasources_changed(entries);
        }
        removed
    }

    /// Return the focused notebook store, initializing a default focused store
    /// on first use for existing single-window callers.
    pub fn get_notebook(&self) -> Arc<NotebookStore> {
        let id = self.focused_notebook_id_or_create();
        self.notebook_for_id(&id)
    }

    /// Return the notebook store for a saved path without changing focus.
    pub fn notebook_for_path(&self, path: impl AsRef<Path>) -> Arc<NotebookStore> {
        let id = NotebookId::for_saved_path(path.as_ref());
        self.notebook_for_id(&id)
    }

    /// Return and focus the notebook store for a saved path.
    pub fn focus_notebook_path(&self, path: impl AsRef<Path>) -> Arc<NotebookStore> {
        let id = NotebookId::for_saved_path(path.as_ref());
        self.alias_focused_default_store_to_id(&id);
        let store = self.notebook_for_id(&id);
        self.set_focused_notebook_id(id);
        store
    }

    /// Focus an existing or lazily-created saved notebook path for implicit operations.
    #[cfg(test)]
    pub(crate) fn set_focused_notebook_path(&self, path: impl AsRef<Path>) {
        let id = NotebookId::for_saved_path(path.as_ref());
        self.set_focused_notebook_id(id);
    }

    /// Return the notebook store for an explicit notebook ID.
    pub fn notebook_for_id(&self, id: &NotebookId) -> Arc<NotebookStore> {
        Arc::clone(
            self.notebooks
                .entry(id.clone())
                .or_insert_with(|| NotebookStore::new(Arc::new(self.save_coordinator.clone())))
                .value(),
        )
    }

    /// Return the notebook store for an optional wire target.
    ///
    /// `notebook_id` accepts the canonical store key (`nb-...`) and, during the
    /// saved-notebook MVP, an absolute notebook path for frontend callers that
    /// already have the loaded path.
    pub fn notebook_for_optional_target(&self, notebook_id: Option<&str>) -> Arc<NotebookStore> {
        let id = notebook_id
            .map(|target| self.resolve_notebook_target(target))
            .unwrap_or_else(|| self.focused_notebook_id_or_create());
        self.notebook_for_id(&id)
    }

    /// Return the focused notebook ID, if one has been established.
    pub fn focused_notebook_id(&self) -> Option<NotebookId> {
        self.focused.lock().clone()
    }

    /// Focus a notebook by canonical store key or saved path.
    pub fn set_focused_notebook_target(&self, notebook_id: &str) -> NotebookId {
        let id = self.resolve_notebook_target(notebook_id);
        self.set_focused_notebook_id(id.clone());
        id
    }

    fn set_focused_notebook_id(&self, id: NotebookId) {
        self.notebook_for_id(&id);
        self.datasource_catalog_for_id(&id);
        *self.focused.lock() = Some(id);
        self.sync_focused_catalog_mirror();
    }

    /// Replace the datasource catalog for a saved path.
    pub(crate) fn replace_datasource_catalog_for_path(
        &self,
        path: impl AsRef<Path>,
        catalog: DatasourceCatalog,
    ) {
        let id = NotebookId::for_saved_path(path.as_ref());
        self.replace_datasource_catalog_for_id(&id, catalog);
    }

    /// Return the focused datasource catalog.
    pub fn focused_datasource_catalog(&self) -> Arc<Mutex<DatasourceCatalog>> {
        let id = self.focused_notebook_id_or_create();
        self.datasource_catalog_for_id(&id)
    }

    /// Return a focused datasource catalog snapshot.
    pub fn list_focused_datasources(&self) -> Vec<DatasourceEntry> {
        self.focused_datasource_catalog().lock().list()
    }

    /// Return a datasource catalog snapshot for an optional wire target.
    pub fn list_datasources_for_optional_target(
        &self,
        notebook_id: Option<&str>,
    ) -> Vec<DatasourceEntry> {
        let id = notebook_id
            .map(|target| self.resolve_notebook_target(target))
            .unwrap_or_else(|| self.focused_notebook_id_or_create());
        self.datasource_catalog_for_id(&id).lock().list()
    }

    fn resolve_notebook_target(&self, target: &str) -> NotebookId {
        self.notebooks
            .iter()
            .find_map(|entry| (entry.key().store_key() == target).then(|| entry.key().clone()))
            .unwrap_or_else(|| NotebookId::for_saved_path(target))
    }

    fn datasource_catalog_for_id(&self, id: &NotebookId) -> Arc<Mutex<DatasourceCatalog>> {
        Arc::clone(
            self.datasource_catalogs
                .entry(id.clone())
                .or_insert_with(|| Arc::new(Mutex::new(DatasourceCatalog::default())))
                .value(),
        )
    }

    fn replace_datasource_catalog_for_id(&self, id: &NotebookId, catalog: DatasourceCatalog) {
        self.datasource_catalogs
            .insert(id.clone(), Arc::new(Mutex::new(catalog)));
        if self.focused.lock().as_ref() == Some(id) {
            self.sync_focused_catalog_mirror();
        }
    }

    fn focused_notebook_id_or_create(&self) -> NotebookId {
        if let Some(id) = self.focused.lock().clone() {
            return id;
        }
        let id = default_notebook_id();
        self.set_focused_notebook_id(id.clone());
        id
    }

    fn alias_focused_default_store_to_id(&self, id: &NotebookId) {
        if self.notebooks.contains_key(id) {
            return;
        }
        let default_id = default_notebook_id();
        if self.focused.lock().as_ref() != Some(&default_id) {
            return;
        }
        let Some(store) = self
            .notebooks
            .get(&default_id)
            .map(|entry| Arc::clone(entry.value()))
        else {
            return;
        };
        if store.path().is_some() {
            return;
        }
        self.notebooks.insert(id.clone(), store);
        if !self.datasource_catalogs.contains_key(id) {
            if let Some(catalog) = self
                .datasource_catalogs
                .get(&default_id)
                .map(|entry| Arc::clone(entry.value()))
            {
                self.datasource_catalogs.insert(id.clone(), catalog);
            }
        }
    }

    fn sync_focused_catalog_mirror(&self) {
        let Some(id) = self.focused.lock().clone() else {
            return;
        };
        let catalog = self.datasource_catalog_for_id(&id).lock().clone();
        *self.datasource_catalog.lock() = catalog;
    }

    pub(crate) fn emit_datasources_changed(&self, entries: Vec<DatasourceEntry>) {
        let _ = self.event_tx.send(DaemonEvent::DatasourcesChanged(entries));
    }
}

fn default_notebook_id() -> NotebookId {
    NotebookId::for_saved_path("__spur_default_focused_notebook__")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{identity::NotebookId, ports};
    use serde_json::json;

    #[test]
    fn saved_notebook_identity_derives_existing_public_formats_consistently() {
        let path = "/tmp/notebooks/demo.ipynb";
        let id = NotebookId::for_saved_path(path);

        assert_eq!(id.store_key(), ports::notebook_id_for_path(path));
        assert_eq!(id.kernel_slot_id(), notebook_slot_id(path));
        assert_eq!(
            id.kernel_slot_id_for_spec("python3"),
            slot_id_for_spec(path, "python3")
        );
        assert_eq!(id.delta_path(), Some(path));
        assert!(ports::notebook_port_root(path).ends_with(id.store_key()));
    }

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
    fn notebook_path_from_slot_id_strips_prefix_and_spec_suffix() {
        let path = "/tmp/notebooks/polyglot.ipynb";

        assert_eq!(
            notebook_path_from_slot_id(&slot_id_for_spec(path, "python3"), "python3"),
            Some(path)
        );
    }

    #[test]
    fn notebook_path_from_slot_id_accepts_unsuffixed_notebook_slot() {
        let path = "/tmp/notebooks/ui#draft.ipynb";

        assert_eq!(
            notebook_path_from_slot_id(&notebook_slot_id(path), "python3"),
            Some(path)
        );
        assert_eq!(notebook_path_from_slot_id("mcp:kernel", "python3"), None);
    }

    #[test]
    fn notebook_store_is_initialized_lazily_and_reused() {
        let state = State::new();

        let first = state.get_notebook();
        let second = state.get_notebook();

        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn focused_notebook_registry_keeps_path_stores_and_catalogs_isolated() {
        let state = State::new();
        let path_a = "/tmp/notebooks/a.ipynb";
        let path_b = "/tmp/notebooks/b.ipynb";

        state.set_focused_notebook_path(path_a);
        let store_a = state.get_notebook();
        state.attach_datasource(DatasourceEntry {
            name: "sales".to_string(),
            path: "/tmp/sales.csv".to_string(),
            kind: crate::commands::DatasourceKind::Csv,
            group: None,
            columns: Vec::new(),
            row_count: None,
            tables: Vec::new(),
        });

        state.set_focused_notebook_path(path_b);
        let store_b = state.get_notebook();
        assert!(!Arc::ptr_eq(&store_a, &store_b));
        assert!(state.list_focused_datasources().is_empty());

        state.attach_datasource(DatasourceEntry {
            name: "inventory".to_string(),
            path: "/tmp/inventory.csv".to_string(),
            kind: crate::commands::DatasourceKind::Csv,
            group: None,
            columns: Vec::new(),
            row_count: None,
            tables: Vec::new(),
        });

        state.set_focused_notebook_path(path_a);
        let entries = state.list_focused_datasources();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "sales");
        assert!(Arc::ptr_eq(&store_a, &state.notebook_for_path(path_a)));

        state.set_focused_notebook_path(path_b);
        let entries = state.list_focused_datasources();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "inventory");
        assert!(Arc::ptr_eq(&store_b, &state.notebook_for_path(path_b)));
    }

    #[test]
    fn comm_owner_records_resolves_removes_and_clears_slot_entries() {
        let state = State::new();

        record_comm_open(&state, "slot-a", "comm-1");
        assert_eq!(slot_for_comm(&state, "comm-1").as_deref(), Some("slot-a"));

        record_comm_open(&state, "slot-b", "comm-1");
        assert_eq!(slot_for_comm(&state, "comm-1").as_deref(), Some("slot-b"));

        remove_comm_owner(&state, "comm-1");
        assert_eq!(slot_for_comm(&state, "comm-1"), None);

        record_comm_open(&state, "slot-a", "comm-2");
        record_comm_open(&state, "slot-a", "comm-3");
        record_comm_open(&state, "slot-b", "comm-4");

        clear_comm_owners_for_slot(&state, "slot-a");

        assert_eq!(slot_for_comm(&state, "comm-2"), None);
        assert_eq!(slot_for_comm(&state, "comm-3"), None);
        assert_eq!(slot_for_comm(&state, "comm-4").as_deref(), Some("slot-b"));
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
