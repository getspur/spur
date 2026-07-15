//! Durable named-local-project catalog and request-routing helpers.

mod module;
mod routing;
mod store;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use module::LocalProjectCatalogMcpModule;
pub use routing::{
    decorate_project_response, extract_project, with_optional_project_schema, LocalProjectAccess,
};
pub use store::LocalProjectCatalogStore;

/// Current on-disk catalog format version.
pub const LOCAL_PROJECT_CATALOG_VERSION: u32 = 1;

/// One durable name-to-root mapping.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalProjectEntry {
    pub name: String,
    pub root: PathBuf,
}

/// One immutable catalog read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalProjectCatalogSnapshot {
    pub version: u32,
    pub generation: u64,
    pub projects: Vec<LocalProjectEntry>,
}

/// Backward-compatible short name for a catalog snapshot.
pub type LocalProjectSnapshot = LocalProjectCatalogSnapshot;

/// Live readiness state derived from the registered root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LocalProjectStatus {
    Ready,
    Unavailable,
}

/// Validator-owned health result. Health is never persisted in the catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalProjectHealth {
    pub status: LocalProjectStatus,
    pub reason: Option<String>,
}

impl LocalProjectHealth {
    #[must_use]
    pub fn ready() -> Self {
        Self {
            status: LocalProjectStatus::Ready,
            reason: None,
        }
    }

    #[must_use]
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            status: LocalProjectStatus::Unavailable,
            reason: Some(reason.into()),
        }
    }

    #[must_use]
    pub fn is_ready(&self) -> bool {
        self.status == LocalProjectStatus::Ready
    }
}

/// Canonical root and live health returned by a composition-specific validator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedLocalProject {
    pub canonical_root: PathBuf,
    pub health: LocalProjectHealth,
}

/// Dependency-neutral validation boundary implemented by graph+analyst composition.
pub trait LocalProjectValidator: Send + Sync {
    fn validate(&self, requested_path: &Path) -> Result<ValidatedLocalProject, LocalProjectError>;
}

/// One successfully resolved explicit request scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedLocalProject {
    pub name: String,
    pub root: PathBuf,
    pub catalog_generation: u64,
}

/// Mutation result returned by add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalProjectAddResult {
    pub changed: bool,
    pub project: LocalProjectEntry,
    pub catalog_generation: u64,
}

/// Mutation result returned by remove.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LocalProjectRemoveResult {
    pub removed: bool,
    pub catalog_generation: u64,
}

/// Live list entry returned to MCP clients.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LocalProjectListEntry {
    pub name: String,
    pub root: PathBuf,
    pub status: LocalProjectStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Live catalog list response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LocalProjectList {
    pub catalog_generation: u64,
    pub projects: Vec<LocalProjectListEntry>,
}

/// Typed failures shared by storage, validation, routing, and MCP composition.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum LocalProjectError {
    #[error("invalid local project request: {reason}")]
    InvalidRequest { reason: String },
    #[error("invalid local project name `{name}`: {reason}")]
    InvalidName { name: String, reason: String },
    #[error("invalid local project path `{}`: {reason}", path.display())]
    InvalidPath { path: PathBuf, reason: String },
    #[error("cannot resolve local project catalog path: {reason}")]
    ConfigUnavailable { reason: String },
    #[error("local project catalog `{}` is unreadable: {reason}", path.display())]
    CatalogRead { path: PathBuf, reason: String },
    #[error("local project catalog `{}` is invalid: {reason}", path.display())]
    CatalogParse { path: PathBuf, reason: String },
    #[error("local project catalog `{}` has unsupported version {version}; expected version 1", path.display())]
    UnsupportedVersion { path: PathBuf, version: u32 },
    #[error("local project catalog `{}` contains duplicate name `{name}`", path.display())]
    DuplicateName { path: PathBuf, name: String },
    #[error("local project catalog `{}` could not be written: {reason}", path.display())]
    CatalogWrite { path: PathBuf, reason: String },
    #[error("local project catalog generation cannot exceed the TOML integer limit")]
    GenerationOverflow,
    #[error("local project `{name}` is already registered at `{}`; pass replace=true to use `{}`", registered_root.display(), requested_root.display())]
    Conflict {
        name: String,
        registered_root: PathBuf,
        requested_root: PathBuf,
    },
    #[error("unknown local project `{name}`")]
    UnknownProject { name: String },
    #[error("local project `{name}` is unavailable: {reason}")]
    ProjectUnavailable { name: String, reason: String },
}

impl LocalProjectError {
    #[must_use]
    pub fn json_rpc_code(&self) -> i32 {
        match self {
            Self::InvalidRequest { .. }
            | Self::InvalidName { .. }
            | Self::InvalidPath { .. }
            | Self::Conflict { .. } => -32602,
            Self::UnknownProject { .. } | Self::ProjectUnavailable { .. } => -32004,
            Self::ConfigUnavailable { .. }
            | Self::CatalogRead { .. }
            | Self::CatalogParse { .. }
            | Self::UnsupportedVersion { .. }
            | Self::DuplicateName { .. }
            | Self::CatalogWrite { .. }
            | Self::GenerationOverflow => -32603,
        }
    }
}

/// Validates the public catalog name grammar without allocating regex state.
pub fn validate_project_name(name: &str) -> Result<(), LocalProjectError> {
    let bytes = name.as_bytes();
    let valid_first = bytes
        .first()
        .is_some_and(|byte| byte.is_ascii_alphanumeric());
    let valid_rest = bytes
        .iter()
        .skip(1)
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if (1..=64).contains(&bytes.len()) && valid_first && valid_rest {
        return Ok(());
    }
    Err(LocalProjectError::InvalidName {
        name: name.to_owned(),
        reason: "expected [A-Za-z0-9][A-Za-z0-9._-]{0,63}".to_owned(),
    })
}

/// Shared resolver used by catalog-enabled graph and analyst modules.
#[derive(Clone)]
pub struct LocalProjectResolver {
    store: LocalProjectCatalogStore,
    validator: std::sync::Arc<dyn LocalProjectValidator>,
}

impl LocalProjectResolver {
    #[must_use]
    pub fn new(
        store: LocalProjectCatalogStore,
        validator: std::sync::Arc<dyn LocalProjectValidator>,
    ) -> Self {
        Self { store, validator }
    }

    #[must_use]
    pub fn store(&self) -> &LocalProjectCatalogStore {
        &self.store
    }

    pub fn resolve(&self, name: &str) -> Result<ResolvedLocalProject, LocalProjectError> {
        validate_project_name(name)?;
        let snapshot = self.store.snapshot()?;
        let entry = snapshot
            .projects
            .iter()
            .find(|entry| entry.name == name)
            .ok_or_else(|| LocalProjectError::UnknownProject {
                name: name.to_owned(),
            })?;
        let validated = self.validator.validate(&entry.root).map_err(|error| {
            LocalProjectError::ProjectUnavailable {
                name: name.to_owned(),
                reason: error.to_string(),
            }
        })?;
        if !validated.health.is_ready() {
            return Err(LocalProjectError::ProjectUnavailable {
                name: name.to_owned(),
                reason: validated
                    .health
                    .reason
                    .unwrap_or_else(|| "registered root is not query-ready".to_owned()),
            });
        }
        Ok(ResolvedLocalProject {
            name: name.to_owned(),
            root: validated.canonical_root,
            catalog_generation: snapshot.generation,
        })
    }

    pub fn list(&self) -> Result<LocalProjectList, LocalProjectError> {
        let snapshot = self.store.snapshot()?;
        let mut projects = snapshot
            .projects
            .into_iter()
            .map(|entry| match self.validator.validate(&entry.root) {
                Ok(validated) => LocalProjectListEntry {
                    name: entry.name,
                    root: entry.root,
                    status: validated.health.status,
                    reason: validated.health.reason,
                },
                Err(error) => LocalProjectListEntry {
                    name: entry.name,
                    root: entry.root,
                    status: LocalProjectStatus::Unavailable,
                    reason: Some(error.to_string()),
                },
            })
            .collect::<Vec<_>>();
        projects.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(LocalProjectList {
            catalog_generation: snapshot.generation,
            projects,
        })
    }
}
