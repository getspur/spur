//! Canonical notebook identity derivation for the Tauri notebook layer.

use std::path::{Path, PathBuf};

use directories::BaseDirs;

/// Stable semantic identity for one saved notebook.
///
/// This MVP identity substrate only constructs IDs for saved notebooks whose
/// path is already normalized by the daemon load/save path. Scratch notebooks
/// and save-as are intentionally left as explicit future lifecycle operations:
/// scratch needs a UUID-backed identity, and save-as must migrate the store,
/// catalog, focus pointer, delta route, and kernel-slot policy in one place.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NotebookId {
    saved_path: String,
    store_key: String,
}

impl NotebookId {
    /// Derive the canonical identity for a saved notebook path.
    pub fn for_saved_path(path: impl AsRef<Path>) -> Self {
        let saved_path = path.as_ref().to_string_lossy().into_owned();
        let digest = blake3::hash(saved_path.as_bytes()).to_hex();
        let store_key = format!("nb-{}", &digest[..24]);
        Self {
            saved_path,
            store_key,
        }
    }

    /// Stable hashed key used for per-notebook storage directories.
    pub fn store_key(&self) -> &str {
        &self.store_key
    }

    /// Saved path represented by this identity.
    pub fn saved_path(&self) -> &str {
        &self.saved_path
    }

    /// Existing raw path tag used by notebook deltas.
    pub fn delta_path(&self) -> Option<&str> {
        Some(&self.saved_path)
    }

    /// Existing in-memory kernel slot ID for this saved notebook.
    pub fn kernel_slot_id(&self) -> String {
        format!("notebook:{}", self.saved_path)
    }

    /// Existing per-kernelspec slot ID for this saved notebook.
    pub fn kernel_slot_id_for_spec(&self, spec_name: &str) -> String {
        format!("{}#{spec_name}", self.kernel_slot_id())
    }

    /// Per-notebook directory used to store SPUR port files and the manifest.
    pub fn port_root(&self) -> PathBuf {
        BaseDirs::new()
            .map(|dirs| {
                dirs.home_dir()
                    .join(".spur/notebooks")
                    .join(&self.store_key)
            })
            .unwrap_or_else(|| PathBuf::from(".spur/notebooks").join(&self.store_key))
    }
}

#[cfg(test)]
mod tests {
    use super::NotebookId;

    #[test]
    fn saved_notebook_identity_uses_existing_public_strings() {
        let id = NotebookId::for_saved_path("/tmp/notebooks/demo.ipynb");

        assert_eq!(id.store_key(), "nb-6f66ec83895b4a8922cfd26a");
        assert_eq!(id.delta_path(), Some("/tmp/notebooks/demo.ipynb"));
        assert_eq!(id.kernel_slot_id(), "notebook:/tmp/notebooks/demo.ipynb");
        assert_eq!(
            id.kernel_slot_id_for_spec("python3"),
            "notebook:/tmp/notebooks/demo.ipynb#python3"
        );
        assert!(id.port_root().ends_with("nb-6f66ec83895b4a8922cfd26a"));
    }
}
