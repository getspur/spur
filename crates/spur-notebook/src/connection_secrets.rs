//! Credential sink for OAuth-acquired secrets.
//!
//! Secrets are stored in a dedicated `~/.spur/credentials.json` file,
//! deliberately separate from `connections.json`, which is read by `DuckDB`
//! extension and must remain secret-free.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use tokio::{io::AsyncWriteExt as _, sync::Mutex};

const CREDENTIALS_FILE_NAME: &str = "credentials.json";

static CREDENTIALS_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[async_trait]
pub trait CredentialSink: Send + Sync {
    async fn store(&self, key: &str, value: &str) -> Result<()>;
    async fn load_all(&self) -> Result<BTreeMap<String, String>>;

    async fn load_into_env(&self) -> Result<usize> {
        let credentials = self.load_all().await?;
        let count = credentials.len();
        for (key, value) in credentials {
            std::env::set_var(key, value);
        }
        Ok(count)
    }
}

#[derive(Default, Serialize, Deserialize)]
struct SecretsFile {
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

pub struct FileCredentialSink {
    path: PathBuf,
}

impl FileCredentialSink {
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn from_home_dir() -> Result<Self> {
        Ok(Self::at(credentials_record_path()?))
    }

    async fn read(&self) -> Result<SecretsFile> {
        let bytes = match tokio::fs::read(&self.path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SecretsFile::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", self.path.display()));
            }
        };
        match serde_json::from_slice(&bytes) {
            Ok(record) => Ok(record),
            Err(_) => Ok(SecretsFile::default()),
        }
    }

    async fn write(&self, record: &SecretsFile) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }

        let bytes = serde_json::to_vec_pretty(record)?;
        let temp_path = self.path.with_file_name(format!(
            ".{}.{}.tmp",
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(CREDENTIALS_FILE_NAME),
            uuid::Uuid::new_v4()
        ));
        write_secret_file(&temp_path, &bytes).await?;
        if let Err(error) = tokio::fs::rename(&temp_path, &self.path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(error).with_context(|| format!("failed to rename {}", self.path.display()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            tokio::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))
                .await
                .with_context(|| format!("failed to chmod {}", self.path.display()))?;
        }
        Ok(())
    }
}

#[async_trait]
impl CredentialSink for FileCredentialSink {
    async fn store(&self, key: &str, value: &str) -> Result<()> {
        let _guard = CREDENTIALS_LOCK.lock().await;
        let mut record = self.read().await?;
        record.secrets.insert(key.to_owned(), value.to_owned());
        self.write(&record).await
    }

    async fn load_all(&self) -> Result<BTreeMap<String, String>> {
        let _guard = CREDENTIALS_LOCK.lock().await;
        Ok(self.read().await?.secrets)
    }
}

pub async fn load_secrets_into_env() -> Result<usize> {
    FileCredentialSink::from_home_dir()?.load_into_env().await
}

fn credentials_record_path() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
    Ok(base_dirs
        .home_dir()
        .join(".spur")
        .join(CREDENTIALS_FILE_NAME))
}

#[cfg(unix)]
async fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .await
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.write_all(bytes)
        .await
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.sync_all()
        .await
        .with_context(|| format!("failed to sync {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
async fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.write_all(bytes)
        .await
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.sync_all()
        .await
        .with_context(|| format!("failed to sync {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn store_then_load_round_trips() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("credentials.json");
        let sink = FileCredentialSink::at(path.clone());
        sink.store("GOOGLE_ADS_REFRESH_TOKEN", "1//abc")
            .await
            .expect("store");
        let all = sink.load_all().await.expect("load");
        assert_eq!(
            all.get("GOOGLE_ADS_REFRESH_TOKEN").map(String::as_str),
            Some("1//abc")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn secrets_file_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("credentials.json");
        let sink = FileCredentialSink::at(path.clone());
        sink.store("K", "v").await.expect("store");
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[tokio::test]
    async fn load_into_env_sets_vars() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("credentials.json");
        let sink = FileCredentialSink::at(path.clone());
        sink.store("SPUR_TEST_SECRET_X", "yes")
            .await
            .expect("store");
        let n = sink.load_into_env().await.expect("load env");
        assert!(n >= 1);
        assert_eq!(
            std::env::var("SPUR_TEST_SECRET_X").ok().as_deref(),
            Some("yes")
        );
    }
}
