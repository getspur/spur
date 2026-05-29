//! Defines state and stores for the Tauri application.

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use dashmap::DashMap;
use parking_lot::Mutex;

use crate::{
    backend::local::LocalKernel,
    commands::{DatasourceEntry, SaveCoordinator},
    notebook_store::NotebookStore,
};

/// Current schema version for notebook datasource catalog entries.
pub const DATASOURCE_CATALOG_SCHEMA_VERSION: u32 = 1;

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
}

/// Stable prefix used for notebook path-derived kernel slots.
pub(crate) const NOTEBOOK_SLOT_PREFIX: &str = "notebook:";

/// Derive the stable in-memory kernel slot ID for a notebook path.
pub fn notebook_slot_id(path: &str) -> String {
    format!("{NOTEBOOK_SLOT_PREFIX}{path}")
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
#[derive(Default)]
pub struct State {
    /// Current kernel slots in the application, keyed by stable slot ID.
    pub kernels: DashMap<String, KernelSlot>,

    /// Coordinator for debounced notebook saves.
    pub save_coordinator: SaveCoordinator,

    /// In-memory datasource catalog for the active notebook.
    pub datasource_catalog: Mutex<DatasourceCatalog>,

    /// Lazily initialized authoritative notebook document store.
    notebook: Mutex<Option<Arc<NotebookStore>>>,
}

impl State {
    /// Create a new state object.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the process-wide notebook store, initializing it on first use.
    pub fn get_notebook(&self) -> Arc<NotebookStore> {
        let mut notebook = self.notebook.lock();
        notebook
            .get_or_insert_with(|| NotebookStore::new(Arc::new(self.save_coordinator.clone())))
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
