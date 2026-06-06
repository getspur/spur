use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

pub mod archive;

pub const SPUR_APP_EXTENSION: &str = "spurapp";
pub const SPUR_APP_MANIFEST: &str = "spur-app.json";
pub const SPUR_APP_ENTRY_NOTEBOOK: &str = "app.ipynb";
pub const SPUR_APP_SCHEMA: &str = "spur.app/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppManifest {
    pub schema: String,
    pub name: String,
    pub entry_notebook: String,
    pub open_mode: String,
    pub runtime: SpurAppRuntime,
    #[serde(default)]
    pub widgets: Vec<SpurAppWidgetAsset>,
    #[serde(default)]
    pub ports: Option<SpurAppPorts>,
    #[serde(default)]
    pub dependencies: SpurAppDependencies,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppRuntime {
    pub jute_min: String,
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppWidgetAsset {
    pub module: String,
    #[serde(default)]
    pub css: Option<String>,
    pub hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppPorts {
    #[serde(default)]
    pub include_snapshots: bool,
    #[serde(default)]
    pub manifest: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppDependencies {
    #[serde(default)]
    pub python: Option<String>,
    #[serde(default)]
    pub deno: Option<String>,
    #[serde(default)]
    pub rust: Option<String>,
    #[serde(default)]
    pub go: Option<String>,
}

impl SpurAppManifest {
    pub fn minimal(name: impl Into<String>, entry_notebook: impl Into<String>) -> Self {
        Self {
            schema: SPUR_APP_SCHEMA.to_string(),
            name: name.into(),
            entry_notebook: entry_notebook.into(),
            open_mode: "app".to_string(),
            runtime: SpurAppRuntime {
                jute_min: "0.1.0".to_string(),
                features: vec![
                    "frontend-cells".to_string(),
                    "anywidget-afm".to_string(),
                    "ports-arrow".to_string(),
                ],
            },
            widgets: Vec::new(),
            ports: None,
            dependencies: SpurAppDependencies::default(),
        }
    }
}

pub fn is_safe_archive_path(raw: &str) -> bool {
    if raw.is_empty() || raw.contains('\\') {
        return false;
    }

    let path = Path::new(raw);
    !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}
