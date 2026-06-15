//! Minimal Spur App manifest surface needed by code compiled into the Tauri crate.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::backend::notebook::NotebookRoot;

pub(crate) const SPUR_APP_MANIFEST: &str = "spur-app.json";
const SPUR_APP_ENTRY_NOTEBOOK: &str = "app.ipynb";
pub(crate) const SPUR_APP_SCHEMA: &str = "spur.app/v1";
const SPUR_APP_METADATA_KEY: &str = "spur_app";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SpurAppManifest {
    pub(crate) schema: String,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) entry_notebook: String,
    pub(crate) open_mode: String,
    pub(crate) runtime: SpurAppRuntime,
    #[serde(default)]
    pub(crate) capabilities: SpurAppCapabilities,
    #[serde(default)]
    pub(crate) mcp_server: Option<SpurAppMcpServer>,
    #[serde(default)]
    pub(crate) skill: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SpurAppCapabilities {
    #[serde(default)]
    pub(crate) ports: Option<SpurAppCapabilityPorts>,
    #[serde(default)]
    pub(crate) canvas_capture: bool,
    #[serde(default)]
    pub(crate) active_output_scripts: bool,
    #[serde(default)]
    pub(crate) artifacts_dir: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SpurAppCapabilityPorts {
    #[serde(default)]
    pub(crate) read: Vec<String>,
    #[serde(default)]
    pub(crate) write: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SpurAppMcpServer {
    #[serde(rename = "type")]
    pub(crate) server_type: String,
    pub(crate) entry: String,
    #[serde(default)]
    pub(crate) requirements: Option<String>,
    #[serde(default)]
    pub(crate) env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SpurAppRuntime {
    pub(crate) jute_min: String,
    #[serde(default)]
    pub(crate) features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManifestSource {
    Embedded,
    SiblingJson(PathBuf),
}

fn absolute_notebook_path(notebook_path: &Path) -> PathBuf {
    if let Ok(path) = std::fs::canonicalize(notebook_path) {
        return path;
    }

    if notebook_path.is_absolute() {
        return notebook_path.to_path_buf();
    }

    std::env::current_dir()
        .map(|cwd| cwd.join(notebook_path))
        .unwrap_or_else(|_| notebook_path.to_path_buf())
}

fn manifest_from_notebook(notebook_path: &Path) -> Option<(PathBuf, SpurAppManifest)> {
    let notebook_path = absolute_notebook_path(notebook_path);
    let raw = fs::read_to_string(&notebook_path).ok()?;
    let root: NotebookRoot = match serde_json::from_str(&raw) {
        Ok(root) => root,
        Err(error) => {
            tracing::warn!(
                %error,
                path = %notebook_path.display(),
                "failed to parse notebook while reading embedded Spur App manifest"
            );
            return None;
        }
    };
    let value = root.metadata.other.get(SPUR_APP_METADATA_KEY)?;

    let mut manifest: SpurAppManifest = match serde_json::from_value(value.clone()) {
        Ok(manifest) => manifest,
        Err(error) => {
            tracing::warn!(
                %error,
                path = %notebook_path.display(),
                "invalid notebook metadata.spur_app manifest"
            );
            return None;
        }
    };
    if manifest.entry_notebook.is_empty() {
        manifest.entry_notebook = notebook_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(SPUR_APP_ENTRY_NOTEBOOK)
            .to_owned();
    }

    let app_root = notebook_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    Some((app_root, manifest))
}

pub(crate) fn resolve_app_manifest(
    notebook_path: &Path,
) -> Option<(PathBuf, SpurAppManifest, ManifestSource)> {
    let notebook_path = absolute_notebook_path(notebook_path);
    let app_root = notebook_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".")
        });

    if let Some((app_root, manifest)) = manifest_from_notebook(&notebook_path) {
        return Some((app_root, manifest, ManifestSource::Embedded));
    }

    let manifest_path = app_root.join(SPUR_APP_MANIFEST);
    let raw = fs::read(&manifest_path).ok()?;
    let manifest = match serde_json::from_slice::<SpurAppManifest>(&raw) {
        Ok(manifest) => manifest,
        Err(error) => {
            tracing::warn!(
                %error,
                path = %manifest_path.display(),
                "invalid sibling spur-app.json manifest"
            );
            return None;
        }
    };

    Some((
        app_root,
        manifest,
        ManifestSource::SiblingJson(manifest_path),
    ))
}
