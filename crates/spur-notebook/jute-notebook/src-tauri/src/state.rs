//! Defines state and stores for the Tauri application.

use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;

use crate::{backend::local::LocalKernel, commands::SaveCoordinator};

/// Stable prefix used for notebook path-derived kernel slots.
pub(crate) const NOTEBOOK_SLOT_PREFIX: &str = "notebook:";

/// Derive the stable in-memory kernel slot ID for a notebook path.
pub(crate) fn notebook_slot_id(path: &str) -> String {
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
    pub(crate) save_coordinator: SaveCoordinator,
}

impl State {
    /// Create a new state object.
    pub fn new() -> Self {
        Self::default()
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
}
