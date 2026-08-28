//! Data contracts for the bronze/silver/gold context-service layout.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const BRONZE_PREFIX: &str = "bronze";
pub const SILVER_PREFIX: &str = "silver";
pub const GOLD_PREFIX: &str = "gold";
pub const GOLD_DATA_PREFIX: &str = "gold/data";
pub const GOLD_CATALOG_SNAPSHOT_PREFIX: &str = "gold/catalog-snapshot";
pub const SILVER_BUNDLE_SCHEMA_VERSION: u32 = 1;
pub const SILVER_MANIFEST_FILENAME: &str = "silver-manifest.json";

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SilverManifest {
    pub schema_hash: String,
    pub files: Vec<SilverManifestFile>,
}

#[derive(Serialize)]
struct SilverManifestRef<'a> {
    schema_version: u32,
    schema_hash: &'a str,
    files: &'a [SilverManifestFile],
}

#[derive(Deserialize)]
struct SilverManifestWire {
    schema_version: u32,
    schema_hash: String,
    files: Vec<SilverManifestFile>,
}

impl Serialize for SilverManifest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        SilverManifestRef {
            schema_version: SILVER_BUNDLE_SCHEMA_VERSION,
            schema_hash: &self.schema_hash,
            files: &self.files,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SilverManifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SilverManifestWire::deserialize(deserializer)?;
        if wire.schema_version != SILVER_BUNDLE_SCHEMA_VERSION {
            return Err(serde::de::Error::custom(format!(
                "unsupported Silver bundle schema version {}; expected {}",
                wire.schema_version, SILVER_BUNDLE_SCHEMA_VERSION
            )));
        }
        Ok(Self {
            schema_hash: wire.schema_hash,
            files: wire.files,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SilverManifestFile {
    pub path: String,
    pub size_bytes: u64,
    pub etag: String,
    pub sha256: String,
}
