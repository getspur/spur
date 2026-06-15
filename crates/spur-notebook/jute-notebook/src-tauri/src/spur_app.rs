//! Narrow Spur App manifest shim for shared sidebar chat scope resolution.

#![allow(missing_docs)]

use std::{
    collections::BTreeMap,
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
    pub mcp_server: Option<SpurAppMcpServer>,
    #[serde(default)]
    pub skill: Option<String>,
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

pub fn manifest_from_notebook(notebook_path: &Path) -> Option<(PathBuf, SpurAppManifest)> {
    let raw = std::fs::read_to_string(notebook_path).ok()?;
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
    let Some(value) = root.metadata.other.get(SPUR_APP_METADATA_KEY) else {
        return None;
    };

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
            .to_string();
    }

    let app_root = notebook_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    Some((app_root, manifest))
}
