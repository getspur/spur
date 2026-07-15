//! Production composition for the user-level local-project catalog.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use spur_mcp::local_projects::{
    LocalProjectCatalogMcpModule, LocalProjectCatalogStore, LocalProjectError, LocalProjectHealth,
    LocalProjectResolver, LocalProjectValidator, ValidatedLocalProject,
};

/// Validator shared by brain and standalone user-facing MCP compositions.
///
/// Registration is deliberately read-only: it resolves a real Git worktree
/// root and verifies that both graph and analyst artifacts already exist.
#[derive(Clone, Debug, Default)]
pub struct IndexedLocalProjectValidator;

impl LocalProjectValidator for IndexedLocalProjectValidator {
    fn validate(&self, requested_path: &Path) -> Result<ValidatedLocalProject, LocalProjectError> {
        let requested_path = requested_path
            .canonicalize()
            .map_err(|error| invalid_path(requested_path, error.to_string()))?;
        if !requested_path.is_dir() {
            return Err(invalid_path(&requested_path, "path must be a directory"));
        }

        let canonical_root = git_worktree_root(&requested_path)?;
        spur_graph::mcp::ensure_graph_artifact_ready(&canonical_root).map_err(|error| {
            invalid_path(
                &canonical_root,
                format!("graph index is unavailable: {error:#}"),
            )
        })?;
        spur_analyst::mcp::ensure_analyst_db_ready(&canonical_root).map_err(|error| {
            invalid_path(
                &canonical_root,
                format!("analyst index is unavailable: {error:#}"),
            )
        })?;

        Ok(ValidatedLocalProject {
            canonical_root,
            health: LocalProjectHealth::ready(),
        })
    }
}

/// One shared catalog store, resolver, validator, and management module.
///
/// Clone this value when composing multiple user-facing modules so every
/// graph, analyst, and management tool observes the same durable catalog.
#[derive(Clone)]
pub struct LocalProjectMcpComposition {
    catalog_module: LocalProjectCatalogMcpModule,
}

impl Default for LocalProjectMcpComposition {
    fn default() -> Self {
        Self::from_environment()
    }
}

impl LocalProjectMcpComposition {
    /// Compose a catalog at the standard environment-resolved user path.
    #[must_use]
    pub fn from_environment() -> Self {
        Self::new(LocalProjectCatalogStore::from_environment())
    }

    /// Compose a catalog around an explicit store, primarily for hermetic
    /// embedding and tests.
    #[must_use]
    pub fn new(store: LocalProjectCatalogStore) -> Self {
        let validator: Arc<dyn LocalProjectValidator> = Arc::new(IndexedLocalProjectValidator);
        Self {
            catalog_module: LocalProjectCatalogMcpModule::new(store, validator),
        }
    }

    /// Clone the real management module for registry composition.
    #[must_use]
    pub fn catalog_module(&self) -> LocalProjectCatalogMcpModule {
        self.catalog_module.clone()
    }

    /// Clone the resolver shared by project-enabled graph and analyst modules.
    #[must_use]
    pub fn resolver(&self) -> LocalProjectResolver {
        self.catalog_module.resolver()
    }
}

fn git_worktree_root(requested_path: &Path) -> Result<PathBuf, LocalProjectError> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(requested_path)
        .output()
        .map_err(|error| invalid_path(requested_path, format!("cannot run git: {error}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(invalid_path(
            requested_path,
            format!("path is not inside a Git worktree: {}", stderr.trim()),
        ));
    }
    let root = String::from_utf8(output.stdout).map_err(|error| {
        invalid_path(
            requested_path,
            format!("Git worktree root is not UTF-8: {error}"),
        )
    })?;
    let root = root.trim();
    if root.is_empty() {
        return Err(invalid_path(
            requested_path,
            "Git returned an empty worktree root",
        ));
    }
    PathBuf::from(root)
        .canonicalize()
        .map_err(|error| invalid_path(requested_path, format!("invalid Git root: {error}")))
}

fn invalid_path(path: &Path, reason: impl Into<String>) -> LocalProjectError {
    LocalProjectError::InvalidPath {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}
