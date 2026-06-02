use std::{
    path::{Path, PathBuf},
    sync::LazyLock,
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use ts_rs::TS;

const CONNECTIONS_FILE_NAME: &str = "connections.json";

static CONNECTIONS_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase")]
pub struct ConnectionTemplate {
    pub name: String,
    pub provider: Option<String>,
    pub group: Option<String>,
    pub manifest_toml: String,
    pub tables: Vec<jute::commands::Table>,
    pub credential_env_vars: Vec<String>,
    #[ts(type = "string")]
    pub created_at: DateTime<Utc>,
    #[ts(type = "string")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ConnectionTemplatesFile {
    #[serde(default)]
    templates: Vec<ConnectionTemplate>,
}

pub async fn list() -> Result<Vec<ConnectionTemplate>> {
    list_at(&connections_record_path()?).await
}

pub async fn upsert(template: ConnectionTemplate) -> Result<()> {
    upsert_at(&connections_record_path()?, template).await
}

pub async fn remove(name: &str) -> Result<()> {
    remove_at(&connections_record_path()?, name).await
}

fn connections_record_path() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
    Ok(base_dirs
        .home_dir()
        .join(".spur")
        .join(CONNECTIONS_FILE_NAME))
}

pub(crate) async fn list_at(record_path: &Path) -> Result<Vec<ConnectionTemplate>> {
    let _guard = CONNECTIONS_LOCK.lock().await;
    Ok(read_connections_at(record_path).await?.templates)
}

pub(crate) async fn upsert_at(record_path: &Path, mut template: ConnectionTemplate) -> Result<()> {
    let _guard = CONNECTIONS_LOCK.lock().await;
    let mut record = read_connections_at(record_path).await?;
    match record
        .templates
        .iter_mut()
        .find(|existing| existing.name == template.name)
    {
        Some(existing) => {
            template.created_at = existing.created_at;
            *existing = template;
        }
        None => record.templates.push(template),
    }
    write_connections_at(record_path, &record).await
}

pub(crate) async fn remove_at(record_path: &Path, name: &str) -> Result<()> {
    let _guard = CONNECTIONS_LOCK.lock().await;
    let mut record = read_connections_at(record_path).await?;
    let original_len = record.templates.len();
    record.templates.retain(|template| template.name != name);
    if record.templates.len() != original_len {
        write_connections_at(record_path, &record).await?;
    }
    Ok(())
}

async fn read_connections_at(record_path: &Path) -> Result<ConnectionTemplatesFile> {
    let bytes = match tokio::fs::read(record_path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ConnectionTemplatesFile {
                templates: Vec::new(),
            });
        }
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", record_path.display()));
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(record) => Ok(record),
        Err(_) => Ok(ConnectionTemplatesFile {
            templates: Vec::new(),
        }),
    }
}

async fn write_connections_at(record_path: &Path, record: &ConnectionTemplatesFile) -> Result<()> {
    if let Some(parent) = record_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let bytes = serde_json::to_vec_pretty(record)?;
    let temp_path = record_path.with_file_name(format!(
        ".{}.{}.tmp",
        record_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(CONNECTIONS_FILE_NAME),
        uuid::Uuid::new_v4()
    ));
    tokio::fs::write(&temp_path, bytes)
        .await
        .with_context(|| format!("failed to write {}", temp_path.display()))?;
    if let Err(error) = tokio::fs::rename(&temp_path, record_path).await {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(error).with_context(|| format!("failed to rename {}", record_path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};
    use jute::commands::Table;
    use std::path::Path;

    fn tempdir(prefix: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(prefix)
            .tempdir()
            .expect("temp dir")
    }

    fn template(name: &str, provider: Option<&str>) -> ConnectionTemplate {
        let now = Utc::now();
        let manifest_toml = match provider {
            Some(provider) => format!("[source]\nprovider = \"{provider}\"\n"),
            None => "[source]\nkind = \"manual\"\n".to_string(),
        };
        let credential_env_vars = provider
            .map(|provider| vec![format!("{}_API_KEY", provider.to_uppercase())])
            .unwrap_or_default();

        ConnectionTemplate {
            name: name.to_string(),
            provider: provider.map(str::to_string),
            group: Some("workspace".to_string()),
            manifest_toml,
            tables: vec![Table {
                name: "orders".to_string(),
                columns: Vec::new(),
                row_count: Some(12),
            }],
            credential_env_vars,
            created_at: now - ChronoDuration::hours(1),
            updated_at: now,
        }
    }

    async fn write_corrupt(path: &Path) {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .expect("parent dir writes");
        }
        tokio::fs::write(path, b"{not valid json")
            .await
            .expect("corrupt file writes");
    }

    #[tokio::test]
    async fn upsert_and_list_roundtrip() {
        let temp_dir = tempdir("spur-connections-roundtrip-");
        let record_path = temp_dir.path().join("connections.json");
        let expected = template("manual", None);

        upsert_at(&record_path, expected.clone())
            .await
            .expect("connection upserts");

        let templates = list_at(&record_path).await.expect("connections list reads");
        assert_eq!(templates, vec![expected]);
        assert!(templates[0].provider.is_none());

        let persisted = tokio::fs::read_to_string(&record_path)
            .await
            .expect("connections file reads");
        assert!(persisted.contains("\"provider\": null"));
        assert!(persisted.contains("\"manifestToml\""));
        assert!(persisted.contains("\"credentialEnvVars\""));
        assert!(persisted.contains("\"createdAt\""));
        assert!(persisted.contains("\"updatedAt\""));
    }

    #[tokio::test]
    async fn upsert_overwrites_by_name_and_preserves_created_at() {
        let temp_dir = tempdir("spur-connections-overwrite-");
        let record_path = temp_dir.path().join("connections.json");
        let original = template("warehouse", Some("stripe"));
        let mut replacement = template("warehouse", Some("github"));
        replacement.group = Some("imports".to_string());
        replacement.manifest_toml = "[source]\nprovider = \"github\"\n".to_string();
        replacement.created_at = Utc::now();
        replacement.updated_at = Utc::now() + ChronoDuration::minutes(5);

        upsert_at(&record_path, original.clone())
            .await
            .expect("first connection upserts");
        upsert_at(&record_path, replacement.clone())
            .await
            .expect("replacement connection upserts");

        let templates = list_at(&record_path).await.expect("connections list reads");
        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].name, "warehouse");
        assert_eq!(templates[0].provider.as_deref(), Some("github"));
        assert_eq!(templates[0].group.as_deref(), Some("imports"));
        assert_eq!(templates[0].manifest_toml, replacement.manifest_toml);
        assert_eq!(templates[0].created_at, original.created_at);
        assert_eq!(templates[0].updated_at, replacement.updated_at);
    }

    #[tokio::test]
    async fn remove_deletes_by_name() {
        let temp_dir = tempdir("spur-connections-remove-");
        let record_path = temp_dir.path().join("connections.json");
        let first = template("warehouse", Some("stripe"));
        let second = template("issues", Some("github"));

        upsert_at(&record_path, first.clone())
            .await
            .expect("first connection upserts");
        upsert_at(&record_path, second.clone())
            .await
            .expect("second connection upserts");
        remove_at(&record_path, "warehouse")
            .await
            .expect("connection removes");

        let templates = list_at(&record_path).await.expect("connections list reads");
        assert_eq!(templates, vec![second]);
    }

    #[tokio::test]
    async fn missing_or_corrupt_file_lists_empty() {
        let temp_dir = tempdir("spur-connections-empty-");
        let missing_path = temp_dir.path().join("missing.json");
        let corrupt_path = temp_dir.path().join("corrupt.json");
        write_corrupt(&corrupt_path).await;

        assert_eq!(
            list_at(&missing_path)
                .await
                .expect("missing file returns empty"),
            Vec::<ConnectionTemplate>::new()
        );
        assert_eq!(
            list_at(&corrupt_path)
                .await
                .expect("corrupt file returns empty"),
            Vec::<ConnectionTemplate>::new()
        );
    }
}
