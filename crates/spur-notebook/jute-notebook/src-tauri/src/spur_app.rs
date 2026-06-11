//! Narrow Spur App manifest shim for shared sidebar chat scope resolution.

#![allow(missing_docs)]

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const SPUR_APP_MANIFEST: &str = "spur-app.json";
pub const SPUR_APP_SCHEMA: &str = "spur.app/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpurAppManifest {
    pub schema: String,
    pub name: String,
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
