//! Spur App manifest resolver shared by Tauri commands and path-included
//! sidebar chat modules.

#![allow(missing_docs)]

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::backend::notebook::NotebookRoot;

pub const SPUR_APP_MANIFEST: &str = "spur-app.json";
pub const SPUR_APP_ENTRY_NOTEBOOK: &str = "app.ipynb";
pub const SPUR_APP_SCHEMA: &str = "spur.app/v1";
pub const SPUR_APP_METADATA_KEY: &str = "spur_app";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppManifest {
    pub schema: String,
    pub name: String,
    #[serde(default)]
    pub entry_notebook: String,
    pub open_mode: String,
    pub runtime: SpurAppRuntime,
    #[serde(default)]
    pub capabilities: SpurAppCapabilities,
    #[serde(default)]
    pub mcp_server: Option<SpurAppMcpServer>,
    #[serde(default)]
    pub skill: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpurAppCapabilities {
    #[serde(default)]
    pub ports: Option<SpurAppCapabilityPorts>,
    #[serde(default)]
    pub canvas_capture: bool,
    #[serde(default)]
    pub active_output_scripts: bool,
    #[serde(default)]
    pub artifacts_dir: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppCapabilityPorts {
    #[serde(default)]
    pub read: Vec<String>,
    #[serde(default)]
    pub write: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppMcpServer {
    #[serde(rename = "type")]
    pub server_type: String,
    pub entry: String,
    #[serde(default)]
    pub requirements: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppRuntime {
    pub jute_min: String,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestSource {
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

pub fn manifest_from_notebook(notebook_path: &Path) -> Option<(PathBuf, SpurAppManifest)> {
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

pub fn resolve_app_manifest(
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
