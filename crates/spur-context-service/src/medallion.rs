//! Data contracts for the bronze/silver/gold context-service layout.

use serde::{Deserialize, Serialize};

pub const BRONZE_PREFIX: &str = "bronze";
pub const SILVER_PREFIX: &str = "silver";
pub const GOLD_PREFIX: &str = "gold";
pub const GOLD_DATA_PREFIX: &str = "gold/data";
pub const GOLD_CATALOG_SNAPSHOT_PREFIX: &str = "gold/catalog-snapshot";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BronzeIdentity {
    pub source: String,
    pub package: String,
    pub version: String,
    pub content_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SilverIdentity {
    pub bronze_content_sha256: String,
    pub builder_version: String,
    pub graph_content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoldIdentity {
    pub silver_graph_content_hash: String,
    pub translate_schema_version: String,
    pub embed_text_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SilverManifest {
    pub schema_hash: String,
    pub files: Vec<SilverManifestFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SilverManifestFile {
    pub path: String,
    pub size_bytes: u64,
    pub etag: String,
}
