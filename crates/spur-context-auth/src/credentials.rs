//! Purpose-separated credential stores and API-key resolution.
//!
//! Secrets are never part of normal SPUR configuration. Normal configuration
//! selects only a profile; a separate runtime store selection may explicitly
//! opt into a restricted fallback file. API-key lookup is environment, OS
//! keyring, then file; management lookup never reads the API-key environment
//! variable.

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;
use tokio::sync::{Mutex, RwLock};
use url::Url;

use crate::oauth::ManagementSession;

/// Credential-store failures with no secret-bearing platform details.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CredentialError {
    /// A profile name or credential value is malformed.
    #[error("invalid credential")]
    InvalidCredential,
    /// The profile namespace and stored credential kind disagree.
    #[error("credential purpose mismatch")]
    PurposeMismatch,
    /// A fallback file is not owner-only or equivalently restricted.
    #[error("credential file permissions are not restricted")]
    InsecureFilePermissions,
    /// The selected platform store is unavailable, so a fallback may be used.
    #[error("credential store unavailable")]
    Unavailable,
    /// Credential persistence failed closed.
    #[error("credential store operation failed")]
    Backend,
}

/// Strict namespace for credentials with different authority.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub enum CredentialPurpose {
    /// Human OAuth tokens accepted only by management commands.
    Management,
    /// Personal API key accepted only by the exact API-key MCP route.
    ApiKey,
}

impl CredentialPurpose {
    const fn prefix(self) -> &'static str {
        match self {
            Self::Management => "management",
            Self::ApiKey => "api-key",
        }
    }
}

/// Validated local credential profile plus its authority namespace.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct CredentialProfile {
    name: String,
    purpose: CredentialPurpose,
}

impl CredentialProfile {
    /// Creates a bounded profile. Names are safe for keyring usernames.
    pub fn new(
        name: impl Into<String>,
        purpose: CredentialPurpose,
    ) -> Result<Self, CredentialError> {
        let name = name.into();
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(CredentialError::InvalidCredential);
        }
        Ok(Self { name, purpose })
    }

    /// Profile namespace.
    #[must_use]
    pub const fn purpose(&self) -> CredentialPurpose {
        self.purpose
    }

    fn storage_key(&self) -> String {
        format!("{}:{}", self.purpose.prefix(), self.name)
    }
}

/// A canonical personal API key imported from one stdin line.
#[derive(Clone)]
pub struct ApiKeyCredential {
    secret: SecretString,
    public_id: String,
}

impl ApiKeyCredential {
    /// Parses exactly one canonical `spur_(live|test)_<id>_<secret>` line.
    pub fn parse_stdin(input: &str) -> Result<Self, CredentialError> {
        let value = input
            .strip_suffix("\r\n")
            .or_else(|| input.strip_suffix('\n'))
            .unwrap_or(input);
        if value.contains(['\r', '\n']) || value.trim() != value {
            return Err(CredentialError::InvalidCredential);
        }
        let mut parts = value.split('_');
        let prefix = parts.next();
        let environment = parts.next();
        let public_id = parts.next();
        let secret = parts.next();
        if prefix != Some("spur")
            || !matches!(environment, Some("live" | "test"))
            || parts.next().is_some()
        {
            return Err(CredentialError::InvalidCredential);
        }
        let (Some(public_id), Some(secret)) = (public_id, secret) else {
            return Err(CredentialError::InvalidCredential);
        };
        if !canonical_base32(public_id, 26) || !canonical_base32(secret, 52) {
            return Err(CredentialError::InvalidCredential);
        }
        Ok(Self {
            secret: SecretString::from(value.to_owned()),
            public_id: public_id.to_owned(),
        })
    }

    /// Full key for the exact `X-SPUR-API-Key` boundary.
    #[must_use]
    pub const fn secret(&self) -> &SecretString {
        &self.secret
    }
    /// Non-secret public key ID.
    #[must_use]
    pub fn public_id(&self) -> &str {
        &self.public_id
    }
}

fn canonical_base32(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte))
}

impl fmt::Debug for ApiKeyCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApiKeyCredential([REDACTED])")
    }
}

impl PartialEq for ApiKeyCredential {
    fn eq(&self, other: &Self) -> bool {
        self.public_id == other.public_id
            && self.secret.expose_secret().len() == other.secret.expose_secret().len()
            && bool::from(
                self.secret
                    .expose_secret()
                    .as_bytes()
                    .ct_eq(other.secret.expose_secret().as_bytes()),
            )
    }
}
impl Eq for ApiKeyCredential {}

/// Persisted human OAuth credentials used only by management commands.
#[derive(Clone)]
pub struct ManagementCredential {
    access_token: SecretString,
    refresh_token: SecretString,
    expires_at: u64,
    issuer: Url,
    client_id: String,
}

impl ManagementCredential {
    /// Creates a validated management credential.
    pub fn new(
        access_token: impl Into<String>,
        refresh_token: impl Into<String>,
        expires_at: u64,
        issuer: impl AsRef<str>,
        client_id: impl Into<String>,
    ) -> Result<Self, CredentialError> {
        let access_token = access_token.into();
        let refresh_token = refresh_token.into();
        let issuer =
            Url::parse(issuer.as_ref()).map_err(|_error| CredentialError::InvalidCredential)?;
        let client_id = client_id.into();
        if access_token.is_empty()
            || refresh_token.is_empty()
            || client_id.trim().is_empty()
            || issuer.scheme() != "https"
            || !issuer.username().is_empty()
            || issuer.password().is_some()
            || issuer.query().is_some()
            || issuer.fragment().is_some()
            || access_token.chars().any(char::is_control)
            || refresh_token.chars().any(char::is_control)
        {
            return Err(CredentialError::InvalidCredential);
        }
        Ok(Self {
            access_token: SecretString::from(access_token),
            refresh_token: SecretString::from(refresh_token),
            expires_at,
            issuer,
            client_id,
        })
    }

    /// Reconstructs the refreshable runtime session after loading from a store.
    pub fn session(&self) -> Result<ManagementSession, CredentialError> {
        ManagementSession::new(
            self.access_token.expose_secret(),
            self.refresh_token.expose_secret(),
            self.expires_at,
            self.issuer.as_str(),
            self.client_id.clone(),
        )
        .map_err(|_error| CredentialError::InvalidCredential)
    }

    /// Validated issuer associated with the persisted session.
    #[must_use]
    pub const fn issuer(&self) -> &Url {
        &self.issuer
    }

    /// Public native-client identifier associated with the session.
    #[must_use]
    pub fn client_id(&self) -> &str {
        &self.client_id
    }
}

impl fmt::Debug for ManagementCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagementCredential")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .finish()
    }
}

impl PartialEq for ManagementCredential {
    fn eq(&self, other: &Self) -> bool {
        self.expires_at == other.expires_at
            && self.issuer == other.issuer
            && self.client_id == other.client_id
            && secret_eq(&self.access_token, &other.access_token)
            && secret_eq(&self.refresh_token, &other.refresh_token)
    }
}
impl Eq for ManagementCredential {}

fn secret_eq(left: &SecretString, right: &SecretString) -> bool {
    left.expose_secret().len() == right.expose_secret().len()
        && bool::from(
            left.expose_secret()
                .as_bytes()
                .ct_eq(right.expose_secret().as_bytes()),
        )
}

/// Purpose-tagged value accepted by [`CredentialStore`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoredCredential {
    /// Human OAuth management credential.
    Management(ManagementCredential),
    /// Personal API-key credential.
    ApiKey(ApiKeyCredential),
}

impl StoredCredential {
    const fn purpose(&self) -> CredentialPurpose {
        match self {
            Self::Management(_) => CredentialPurpose::Management,
            Self::ApiKey(_) => CredentialPurpose::ApiKey,
        }
    }
}

/// Async abstraction over keyring, restricted-file, and in-memory stores.
#[async_trait]
pub trait CredentialStore: Send + Sync {
    /// Loads a purpose-separated profile.
    async fn load(
        &self,
        profile: &CredentialProfile,
    ) -> Result<Option<StoredCredential>, CredentialError>;
    /// Stores a value only when its purpose matches the profile namespace.
    async fn store(
        &self,
        profile: &CredentialProfile,
        value: &StoredCredential,
    ) -> Result<(), CredentialError>;
    /// Deletes one purpose-separated profile.
    async fn delete(&self, profile: &CredentialProfile) -> Result<(), CredentialError>;
}

/// Deterministic credential store for unit tests and embedding.
#[derive(Default)]
pub struct InMemoryCredentialStore {
    values: RwLock<HashMap<CredentialProfile, StoredCredential>>,
}

#[async_trait]
impl CredentialStore for InMemoryCredentialStore {
    async fn load(
        &self,
        profile: &CredentialProfile,
    ) -> Result<Option<StoredCredential>, CredentialError> {
        Ok(self.values.read().await.get(profile).cloned())
    }
    async fn store(
        &self,
        profile: &CredentialProfile,
        value: &StoredCredential,
    ) -> Result<(), CredentialError> {
        validate_purpose(profile, value)?;
        self.values
            .write()
            .await
            .insert(profile.clone(), value.clone());
        Ok(())
    }
    async fn delete(&self, profile: &CredentialProfile) -> Result<(), CredentialError> {
        self.values.write().await.remove(profile);
        Ok(())
    }
}

/// Cross-platform OS credential-store adapter backed by `keyring`.
#[derive(Clone, Debug)]
pub struct OsKeyringCredentialStore {
    service: Arc<str>,
}

impl OsKeyringCredentialStore {
    /// Creates a keyring namespace. `service` is non-secret application metadata.
    pub fn new(service: impl Into<String>) -> Result<Self, CredentialError> {
        let service = service.into();
        if service.trim().is_empty() || service.len() > 128 || service.chars().any(char::is_control)
        {
            return Err(CredentialError::InvalidCredential);
        }
        Ok(Self {
            service: Arc::from(service),
        })
    }
}

#[async_trait]
impl CredentialStore for OsKeyringCredentialStore {
    async fn load(
        &self,
        profile: &CredentialProfile,
    ) -> Result<Option<StoredCredential>, CredentialError> {
        let service = Arc::clone(&self.service);
        let key = profile.storage_key();
        let expected = profile.purpose;
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(&service, &key)
                .map_err(|_error| CredentialError::Unavailable)?;
            match entry.get_password() {
                Ok(value) => decode_credential(&value, expected).map(Some),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(_) => Err(CredentialError::Unavailable),
            }
        })
        .await
        .map_err(|_error| CredentialError::Backend)?
    }
    async fn store(
        &self,
        profile: &CredentialProfile,
        value: &StoredCredential,
    ) -> Result<(), CredentialError> {
        validate_purpose(profile, value)?;
        let service = Arc::clone(&self.service);
        let key = profile.storage_key();
        let encoded = encode_credential(value)?;
        tokio::task::spawn_blocking(move || {
            keyring::Entry::new(&service, &key)
                .and_then(|entry| entry.set_password(&encoded))
                .map_err(|_error| CredentialError::Unavailable)
        })
        .await
        .map_err(|_error| CredentialError::Backend)?
    }
    async fn delete(&self, profile: &CredentialProfile) -> Result<(), CredentialError> {
        let service = Arc::clone(&self.service);
        let key = profile.storage_key();
        tokio::task::spawn_blocking(move || {
            let entry = keyring::Entry::new(&service, &key)
                .map_err(|_error| CredentialError::Unavailable)?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(_) => Err(CredentialError::Unavailable),
            }
        })
        .await
        .map_err(|_error| CredentialError::Backend)?
    }
}

/// Explicit JSON fallback whose file must be owner-only on every platform.
///
/// The adapter serializes operations within one instance. Callers running
/// multiple processes against the same fallback path must provide a
/// single-writer boundary; the OS keyring remains the preferred concurrent
/// store. Unix requires mode `0600`. Windows requires the current user to own
/// the file and a protected DACL containing exactly one direct read/write ACE
/// for that user. Existing insecure files are rejected, never auto-restricted.
#[derive(Clone, Debug)]
pub struct RestrictedFileCredentialStore {
    path: PathBuf,
    gate: Arc<Mutex<()>>,
}

impl RestrictedFileCredentialStore {
    /// Selects an explicit fallback path. This is deliberately not normal config.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            gate: Arc::new(Mutex::new(())),
        }
    }
}

#[async_trait]
impl CredentialStore for RestrictedFileCredentialStore {
    async fn load(
        &self,
        profile: &CredentialProfile,
    ) -> Result<Option<StoredCredential>, CredentialError> {
        let _guard = self.gate.lock().await;
        let path = self.path.clone();
        let key = profile.storage_key();
        let expected = profile.purpose;
        tokio::task::spawn_blocking(move || {
            let values = read_file(&path)?;
            values
                .get(&key)
                .map(|value| decode_credential(value, expected))
                .transpose()
        })
        .await
        .map_err(|_error| CredentialError::Backend)?
    }
    async fn store(
        &self,
        profile: &CredentialProfile,
        value: &StoredCredential,
    ) -> Result<(), CredentialError> {
        validate_purpose(profile, value)?;
        let _guard = self.gate.lock().await;
        let path = self.path.clone();
        let key = profile.storage_key();
        let encoded = encode_credential(value)?;
        tokio::task::spawn_blocking(move || {
            let mut values = read_file(&path)?;
            values.insert(key, encoded);
            write_file(&path, &values)
        })
        .await
        .map_err(|_error| CredentialError::Backend)?
    }
    async fn delete(&self, profile: &CredentialProfile) -> Result<(), CredentialError> {
        let _guard = self.gate.lock().await;
        let path = self.path.clone();
        let key = profile.storage_key();
        tokio::task::spawn_blocking(move || {
            let mut values = read_file(&path)?;
            if values.remove(&key).is_some() {
                write_file(&path, &values)?;
            }
            Ok(())
        })
        .await
        .map_err(|_error| CredentialError::Backend)?
    }
}

fn validate_purpose(
    profile: &CredentialProfile,
    value: &StoredCredential,
) -> Result<(), CredentialError> {
    if profile.purpose != value.purpose() {
        return Err(CredentialError::PurposeMismatch);
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireCredential {
    Management {
        access_token: String,
        refresh_token: String,
        expires_at: u64,
        issuer: String,
        client_id: String,
    },
    ApiKey {
        key: String,
    },
}

fn encode_credential(value: &StoredCredential) -> Result<String, CredentialError> {
    let wire = match value {
        StoredCredential::Management(value) => WireCredential::Management {
            access_token: value.access_token.expose_secret().to_owned(),
            refresh_token: value.refresh_token.expose_secret().to_owned(),
            expires_at: value.expires_at,
            issuer: value.issuer.to_string(),
            client_id: value.client_id.clone(),
        },
        StoredCredential::ApiKey(value) => WireCredential::ApiKey {
            key: value.secret.expose_secret().to_owned(),
        },
    };
    serde_json::to_string(&wire).map_err(|_error| CredentialError::Backend)
}

fn decode_credential(
    value: &str,
    expected: CredentialPurpose,
) -> Result<StoredCredential, CredentialError> {
    let wire: WireCredential =
        serde_json::from_str(value).map_err(|_error| CredentialError::InvalidCredential)?;
    let stored = match wire {
        WireCredential::Management {
            access_token,
            refresh_token,
            expires_at,
            issuer,
            client_id,
        } => StoredCredential::Management(ManagementCredential::new(
            access_token,
            refresh_token,
            expires_at,
            issuer,
            client_id,
        )?),
        WireCredential::ApiKey { key } => {
            StoredCredential::ApiKey(ApiKeyCredential::parse_stdin(&key)?)
        }
    };
    if stored.purpose() != expected {
        return Err(CredentialError::PurposeMismatch);
    }
    Ok(stored)
}

fn read_file(path: &Path) -> Result<BTreeMap<String, String>, CredentialError> {
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    enforce_restricted(path)?;
    let bytes = std::fs::read(path).map_err(|_error| CredentialError::Backend)?;
    if bytes.len() > 1024 * 1024 {
        return Err(CredentialError::Backend);
    }
    serde_json::from_slice(&bytes).map_err(|_error| CredentialError::InvalidCredential)
}

fn write_file(path: &Path, values: &BTreeMap<String, String>) -> Result<(), CredentialError> {
    let parent = path.parent().ok_or(CredentialError::Backend)?;
    std::fs::create_dir_all(parent).map_err(|_error| CredentialError::Backend)?;
    let bytes = serde_json::to_vec(values).map_err(|_error| CredentialError::Backend)?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|_error| CredentialError::Backend)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|_error| CredentialError::Backend)?;
    }
    #[cfg(windows)]
    windows_file::restrict_new_file(temporary.path())?;
    use std::io::Write as _;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|_error| CredentialError::Backend)?;
    temporary
        .persist(path)
        .map_err(|_error| CredentialError::Backend)?;
    enforce_restricted(path)
}

#[cfg(unix)]
fn enforce_restricted(path: &Path) -> Result<(), CredentialError> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = std::fs::symlink_metadata(path).map_err(|_error| CredentialError::Backend)?;
    if !metadata.file_type().is_file() || metadata.mode() & 0o777 != 0o600 {
        return Err(CredentialError::InsecureFilePermissions);
    }
    Ok(())
}

#[cfg(windows)]
fn enforce_restricted(path: &Path) -> Result<(), CredentialError> {
    windows_file::enforce_restricted(path)
}

#[cfg(all(not(unix), not(windows)))]
fn enforce_restricted(_path: &Path) -> Result<(), CredentialError> {
    Err(CredentialError::InsecureFilePermissions)
}

#[cfg(any(test, windows))]
fn windows_acl_is_owner_only(
    owner_is_current: bool,
    dacl_is_protected: bool,
    ace_matches: &[bool],
) -> bool {
    owner_is_current && dacl_is_protected && ace_matches == [true]
}

#[cfg(windows)]
#[path = "credentials/windows.rs"]
mod windows_file;

/// Resolves an API key as environment, keyring, then explicit restricted file.
pub async fn resolve_api_key(
    profile: &CredentialProfile,
    environment_value: Option<&str>,
    keyring: &dyn CredentialStore,
    file: Option<&dyn CredentialStore>,
) -> Result<Option<ApiKeyCredential>, CredentialError> {
    if profile.purpose != CredentialPurpose::ApiKey {
        return Err(CredentialError::PurposeMismatch);
    }
    if let Some(value) = environment_value {
        return ApiKeyCredential::parse_stdin(value).map(Some);
    }
    match keyring.load(profile).await {
        Ok(Some(StoredCredential::ApiKey(value))) => return Ok(Some(value)),
        Ok(Some(StoredCredential::Management(_))) => return Err(CredentialError::PurposeMismatch),
        Ok(None) | Err(CredentialError::Unavailable) => {}
        Err(error) => return Err(error),
    }
    load_api_key_from(file, profile).await
}

async fn load_api_key_from(
    file: Option<&dyn CredentialStore>,
    profile: &CredentialProfile,
) -> Result<Option<ApiKeyCredential>, CredentialError> {
    match file {
        None => Ok(None),
        Some(file) => match file.load(profile).await? {
            Some(StoredCredential::ApiKey(value)) => Ok(Some(value)),
            Some(StoredCredential::Management(_)) => Err(CredentialError::PurposeMismatch),
            None => Ok(None),
        },
    }
}

/// Resolves management credentials from keyring then explicit file, never env.
pub async fn resolve_management(
    profile: &CredentialProfile,
    keyring: &dyn CredentialStore,
    file: Option<&dyn CredentialStore>,
) -> Result<Option<ManagementCredential>, CredentialError> {
    if profile.purpose != CredentialPurpose::Management {
        return Err(CredentialError::PurposeMismatch);
    }
    match keyring.load(profile).await {
        Ok(Some(StoredCredential::Management(value))) => return Ok(Some(value)),
        Ok(Some(StoredCredential::ApiKey(_))) => return Err(CredentialError::PurposeMismatch),
        Ok(None) | Err(CredentialError::Unavailable) => {}
        Err(error) => return Err(error),
    }
    match file {
        None => Ok(None),
        Some(file) => match file.load(profile).await? {
            Some(StoredCredential::Management(value)) => Ok(Some(value)),
            Some(StoredCredential::ApiKey(_)) => Err(CredentialError::PurposeMismatch),
            None => Ok(None),
        },
    }
}

/// Explicit context-service authentication mode.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextServiceAuth {
    /// Preserve an explicitly anonymous or legacy unauthenticated route.
    None,
    /// Use a refreshed human OAuth bearer token.
    OAuthBearer,
    /// Use a personal API key on the exact API-key MCP route.
    ApiKey,
}

/// Approved non-secret normal context-service configuration.
///
/// Restricted-file fallback is intentionally excluded from serialized normal
/// configuration. Select it explicitly with [`CredentialStoreSelection`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CredentialSelection {
    service_url: Url,
    profile: String,
    auth_mode: ContextServiceAuth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    public_id_hint: Option<String>,
}

impl CredentialSelection {
    /// Creates non-secret normal configuration metadata.
    pub fn new(
        service_url: Url,
        profile: impl Into<String>,
        auth_mode: ContextServiceAuth,
        public_id_hint: Option<String>,
    ) -> Result<Self, CredentialError> {
        let profile = profile.into();
        CredentialProfile::new(profile.clone(), CredentialPurpose::ApiKey)?;
        if service_url.scheme() != "https"
            || service_url.host_str().is_none()
            || !service_url.username().is_empty()
            || service_url.password().is_some()
            || service_url.query().is_some()
            || service_url.fragment().is_some()
            || !matches!(service_url.path(), "" | "/")
            || public_id_hint
                .as_deref()
                .is_some_and(|public_id| !canonical_base32(public_id, 26))
        {
            return Err(CredentialError::InvalidCredential);
        }
        Ok(Self {
            service_url,
            profile,
            auth_mode,
            public_id_hint,
        })
    }

    /// Configured context-service discovery origin.
    #[must_use]
    pub const fn service_url(&self) -> &Url {
        &self.service_url
    }

    /// Selected credential profile name.
    #[must_use]
    pub fn profile(&self) -> &str {
        &self.profile
    }

    /// Explicit mutually exclusive authentication mode.
    #[must_use]
    pub const fn auth_mode(&self) -> ContextServiceAuth {
        self.auth_mode
    }

    /// Optional non-secret public key identifier hint.
    #[must_use]
    pub fn public_id_hint(&self) -> Option<&str> {
        self.public_id_hint.as_deref()
    }
}

/// Explicit runtime selection for credential-store fallback behavior.
///
/// This value is deliberately not serializable as normal context-service
/// configuration. The OS keyring is always primary; a restricted file is used
/// only when the caller explicitly opts into a fallback path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CredentialStoreSelection {
    restricted_file: Option<PathBuf>,
}

impl CredentialStoreSelection {
    /// Uses only the platform keyring.
    #[must_use]
    pub const fn keyring_only() -> Self {
        Self {
            restricted_file: None,
        }
    }

    /// Uses the platform keyring, with one explicit restricted-file fallback.
    pub fn with_restricted_file(path: impl Into<PathBuf>) -> Result<Self, CredentialError> {
        let path = path.into();
        if path.as_os_str().is_empty() {
            return Err(CredentialError::InvalidCredential);
        }
        Ok(Self {
            restricted_file: Some(path),
        })
    }

    /// Explicit restricted-file fallback path, if selected.
    #[must_use]
    pub fn restricted_file(&self) -> Option<&Path> {
        self.restricted_file.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::windows_acl_is_owner_only;

    #[test]
    fn windows_acl_policy_requires_current_owner_and_one_direct_owner_ace() {
        assert!(windows_acl_is_owner_only(true, true, &[true]));
        assert!(!windows_acl_is_owner_only(false, true, &[true]));
        assert!(!windows_acl_is_owner_only(true, false, &[true]));
        assert!(!windows_acl_is_owner_only(true, true, &[]));
        assert!(!windows_acl_is_owner_only(true, true, &[true, true]));
        assert!(!windows_acl_is_owner_only(true, true, &[false]));
    }
}
