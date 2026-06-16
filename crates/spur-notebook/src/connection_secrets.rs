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
use chrono::{DateTime, Utc};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use tokio::{io::AsyncWriteExt as _, sync::Mutex};

const CREDENTIALS_FILE_NAME: &str = "credentials.json";
const CREDENTIAL_PROFILE_VERSION: u32 = 1;

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

#[derive(Debug, Clone)]
pub struct NewCredentialProfile {
    pub id: Option<String>,
    pub provider: String,
    pub label: String,
    pub values: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CredentialProfileSummary {
    pub id: String,
    pub provider: String,
    pub label: String,
    pub keys: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CredentialProfileFile {
    version: u32,
    id: String,
    provider: String,
    label: String,
    values: BTreeMap<String, String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

pub struct FileCredentialProfileStore {
    root: PathBuf,
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

impl CredentialProfileFile {
    fn summary(&self) -> CredentialProfileSummary {
        CredentialProfileSummary {
            id: self.id.clone(),
            provider: self.provider.clone(),
            label: self.label.clone(),
            keys: self.values.keys().cloned().collect(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

impl FileCredentialProfileStore {
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn from_home_dir() -> Result<Self> {
        let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
        Ok(Self::at(
            base_dirs
                .home_dir()
                .join(".spur")
                .join("gateway")
                .join("credential"),
        ))
    }

    pub fn profile_path(&self, id: &str) -> PathBuf {
        self.root.join(format!("{}.json", safe_profile_id(id)))
    }

    pub async fn upsert_profile(
        &self,
        profile: NewCredentialProfile,
    ) -> Result<CredentialProfileSummary> {
        let _guard = CREDENTIALS_LOCK.lock().await;
        let id = profile
            .id
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| default_profile_id(&profile.provider, &profile.label));
        let path = self.profile_path(&id);
        let now = Utc::now();
        let created_at = match self.read_profile_at(&path).await? {
            Some(existing) => existing.created_at,
            None => now,
        };
        let record = CredentialProfileFile {
            version: CREDENTIAL_PROFILE_VERSION,
            id,
            provider: profile.provider,
            label: profile.label,
            values: profile.values,
            created_at,
            updated_at: now,
        };
        self.write_profile(&path, &record).await?;
        Ok(record.summary())
    }

    pub async fn list_summaries(
        &self,
        provider: Option<&str>,
    ) -> Result<Vec<CredentialProfileSummary>> {
        let _guard = CREDENTIALS_LOCK.lock().await;
        let mut entries = match tokio::fs::read_dir(&self.root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", self.root.display()));
            }
        };

        let mut summaries = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .with_context(|| format!("failed to read {}", self.root.display()))?
        {
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Some(profile) = self.read_profile_at(&entry.path()).await? else {
                continue;
            };
            if provider.is_some_and(|provider| profile.provider != provider) {
                continue;
            }
            summaries.push(profile.summary());
        }
        summaries.sort_by(|left, right| {
            left.provider
                .cmp(&right.provider)
                .then(left.label.cmp(&right.label))
                .then(left.id.cmp(&right.id))
        });
        Ok(summaries)
    }

    pub async fn load_values(&self, id: &str) -> Result<Option<BTreeMap<String, String>>> {
        let _guard = CREDENTIALS_LOCK.lock().await;
        Ok(self
            .read_profile_at(&self.profile_path(id))
            .await?
            .map(|profile| profile.values))
    }

    pub async fn load_all_values(&self) -> Result<BTreeMap<String, String>> {
        let _guard = CREDENTIALS_LOCK.lock().await;
        let mut entries = match tokio::fs::read_dir(&self.root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", self.root.display()));
            }
        };

        let mut profiles = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .with_context(|| format!("failed to read {}", self.root.display()))?
        {
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            if let Some(profile) = self.read_profile_at(&entry.path()).await? {
                profiles.push(profile);
            }
        }

        profiles.sort_by(|left, right| {
            left.updated_at
                .cmp(&right.updated_at)
                .then(left.provider.cmp(&right.provider))
                .then(left.label.cmp(&right.label))
                .then(left.id.cmp(&right.id))
        });

        let mut values = BTreeMap::new();
        for profile in profiles {
            values.extend(profile.values);
        }
        Ok(values)
    }

    pub async fn load_into_env(&self) -> Result<usize> {
        let values = self.load_all_values().await?;
        let count = values.len();
        for (key, value) in values {
            std::env::set_var(key, value);
        }
        Ok(count)
    }

    async fn read_profile_at(&self, path: &Path) -> Result<Option<CredentialProfileFile>> {
        let bytes = match tokio::fs::read(path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .with_context(|| format!("failed to decode {}", path.display()))
    }

    async fn write_profile(&self, path: &Path, record: &CredentialProfileFile) -> Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("failed to create {}", parent.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;

                tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                    .await
                    .with_context(|| format!("failed to chmod {}", parent.display()))?;
            }
        }

        let bytes = serde_json::to_vec_pretty(record)?;
        let temp_path = path.with_file_name(format!(
            ".{}.{}.tmp",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("credential.json"),
            uuid::Uuid::new_v4()
        ));
        write_secret_file(&temp_path, &bytes).await?;
        if let Err(error) = tokio::fs::rename(&temp_path, path).await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(error).with_context(|| format!("failed to rename {}", path.display()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .await
                .with_context(|| format!("failed to chmod {}", path.display()))?;
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

pub async fn load_credential_profiles_into_env() -> Result<usize> {
    FileCredentialProfileStore::from_home_dir()?
        .load_into_env()
        .await
}

pub async fn list_credential_profiles(
    provider: Option<&str>,
) -> Result<Vec<CredentialProfileSummary>> {
    FileCredentialProfileStore::from_home_dir()?
        .list_summaries(provider)
        .await
}

fn credentials_record_path() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not resolve home directory")?;
    Ok(base_dirs
        .home_dir()
        .join(".spur")
        .join(CREDENTIALS_FILE_NAME))
}

fn default_profile_id(provider: &str, label: &str) -> String {
    let provider = slug_component(provider);
    let label = slug_component(label);
    match (provider.is_empty(), label.is_empty()) {
        (true, true) => format!("credential-{}", uuid::Uuid::new_v4()),
        (false, true) => provider,
        (true, false) => label,
        (false, false) => format!("{provider}-{label}"),
    }
}

fn safe_profile_id(id: &str) -> String {
    let slug = slug_component(id);
    if slug.is_empty() {
        "credential".to_owned()
    } else {
        slug
    }
}

fn slug_component(value: &str) -> String {
    let mut slug = String::with_capacity(value.len());
    let mut last_was_dash = false;
    for ch in value.trim().chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if matches!(ch, '-' | '_' | ' ' | '.') {
            Some('-')
        } else {
            None
        };
        let Some(ch) = mapped else {
            continue;
        };
        if ch == '-' {
            if last_was_dash || slug.is_empty() {
                continue;
            }
            last_was_dash = true;
        } else {
            last_was_dash = false;
        }
        slug.push(ch);
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
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

    #[tokio::test]
    async fn credential_profile_store_round_trips_provider_profile() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = FileCredentialProfileStore::at(dir.path().join("gateway").join("credential"));
        let mut values = BTreeMap::new();
        values.insert("STRIPE_API_KEY".to_string(), "sk_test_123".to_string());

        let summary = store
            .upsert_profile(NewCredentialProfile {
                id: None,
                provider: "stripe".to_string(),
                label: "Stripe test".to_string(),
                values,
            })
            .await
            .expect("profile stores");

        assert_eq!(summary.provider, "stripe");
        assert_eq!(summary.label, "Stripe test");
        assert_eq!(summary.keys, ["STRIPE_API_KEY"]);
        assert!(!serde_json::to_value(&summary)
            .expect("summary serializes")
            .to_string()
            .contains("sk_test_123"));

        let loaded = store
            .load_values(&summary.id)
            .await
            .expect("profile loads")
            .expect("profile exists");
        assert_eq!(
            loaded.get("STRIPE_API_KEY").map(String::as_str),
            Some("sk_test_123")
        );
    }

    #[tokio::test]
    async fn credential_profile_store_loads_saved_values_into_process_env() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = FileCredentialProfileStore::at(dir.path().join("gateway").join("credential"));
        let token_key = format!(
            "SPUR_TEST_GITHUB_TOKEN_{}",
            uuid::Uuid::new_v4().to_string().replace('-', "_")
        );
        let token_value = "ghp_saved_profile_token";

        let summary = store
            .upsert_profile(NewCredentialProfile {
                id: None,
                provider: "github".to_string(),
                label: "GitHub saved".to_string(),
                values: BTreeMap::from([(token_key.clone(), token_value.to_string())]),
            })
            .await
            .expect("profile stores");

        assert!(!serde_json::to_value(&summary)
            .expect("summary serializes")
            .to_string()
            .contains(token_value));

        let loaded = store
            .load_into_env()
            .await
            .expect("profile values load into process env");

        assert_eq!(loaded, 1);
        assert_eq!(std::env::var(&token_key).ok().as_deref(), Some(token_value));

        let output_path = dir.path().join("kernel-child-env.txt");
        let script = format!("printf '%s' \"${{{token_key}}}\" > \"$1\"");
        let status = tokio::process::Command::new("sh")
            .args([
                "-c",
                script.as_str(),
                "kernel-env-probe",
                output_path.to_string_lossy().as_ref(),
            ])
            .status()
            .await
            .expect("spawn child env probe");
        assert!(status.success());
        assert_eq!(
            tokio::fs::read_to_string(&output_path)
                .await
                .expect("read child env output"),
            token_value
        );
        std::env::remove_var(token_key);
    }

    #[tokio::test]
    async fn credential_profiles_are_filtered_by_provider() {
        let dir = tempfile::tempdir().expect("tmp");
        let store = FileCredentialProfileStore::at(dir.path().join("gateway").join("credential"));

        for (provider, label, key, value) in [
            ("stripe", "Stripe live", "STRIPE_API_KEY", "sk_live"),
            ("github", "GitHub work", "GITHUB_TOKEN", "ghp_work"),
        ] {
            store
                .upsert_profile(NewCredentialProfile {
                    id: None,
                    provider: provider.to_string(),
                    label: label.to_string(),
                    values: BTreeMap::from([(key.to_string(), value.to_string())]),
                })
                .await
                .expect("profile stores");
        }

        let stripe_profiles = store
            .list_summaries(Some("stripe"))
            .await
            .expect("profiles list");
        assert_eq!(stripe_profiles.len(), 1);
        assert_eq!(stripe_profiles[0].label, "Stripe live");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn credential_profile_files_are_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tmp");
        let store = FileCredentialProfileStore::at(dir.path().join("gateway").join("credential"));
        let summary = store
            .upsert_profile(NewCredentialProfile {
                id: None,
                provider: "stripe".to_string(),
                label: "Stripe live".to_string(),
                values: BTreeMap::from([("STRIPE_API_KEY".to_string(), "sk_live".to_string())]),
            })
            .await
            .expect("profile stores");

        let mode = std::fs::metadata(store.profile_path(&summary.id))
            .expect("profile metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
