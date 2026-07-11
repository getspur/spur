//! API-key generation, validation, and durable records.
//!
//! Keys use the fixed `spur_<environment>_<public-id>_<secret>` grammar. Only
//! the SHA-256 digest of the decoded secret bytes belongs in persistent state.
//!
//! ```
//! use secrecy::ExposeSecret;
//! use spur_context_service::api_keys::{
//!     generate_api_key, parse_api_key, verify_secret, ApiKeyScopes, KeyEnvironment,
//! };
//!
//! let generated = generate_api_key(
//!     KeyEnvironment::Test,
//!     "owner",
//!     "CI key",
//!     ApiKeyScopes::parse(&["external.read"]).unwrap(),
//!     1_700_000_000,
//!     1_700_086_400,
//! ).unwrap();
//! let parsed = parse_api_key(generated.plaintext.expose_secret()).unwrap();
//! assert!(verify_secret(&parsed, &generated.record.secret_hash));
//! ```

use std::{
    collections::{BTreeSet, HashMap},
    env, fmt,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use aws_sdk_dynamodb::{
    error::SdkError,
    operation::{transact_write_items::TransactWriteItemsError, update_item::UpdateItemError},
    primitives::Blob,
    types::{AttributeValue, Put, ReturnValue, TransactWriteItem, Update},
    Client as DynamoDbClient,
};
use rand::{rngs::OsRng, RngCore};
use secrecy::SecretString;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

const BASE32_ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
const PUBLIC_ID_BYTES: usize = 16;
const PUBLIC_ID_LEN: usize = 26;
const SECRET_BYTES: usize = 32;
const SECRET_LEN: usize = 52;
const SORTABLE_TIMESTAMP_WIDTH: usize = 20;
const MAX_OWNER_ID_LEN: usize = 200;
const MAX_NAME_LEN: usize = 100;
const MAX_ACTIVE_KEYS_PER_OWNER: u64 = 10;
pub(crate) const MAX_PAGE_LIMIT: usize = 100;
pub(crate) const MAX_SWEEP_BUCKETS: usize = 8;
pub(crate) const MAX_SWEEP_PAGES: usize = 16;
pub(crate) const MAX_SWEEP_RECORDS: usize = 100;
const TTL_GRACE_SECONDS: u64 = 7 * 24 * 60 * 60;
const DEFAULT_API_KEYS_TABLE: &str = "spur-context-api-keys";
/// Delay after an hour closes before its eventually-consistent expiry index
/// bucket may advance the durable high-water cursor.
const EXPIRY_GSI_GRACE_SECONDS: u64 = 60;
/// Recently completed buckets rescanned on every invocation. Combined with the
/// grace above, this bounds index-lag exposure without consuming forward
/// catch-up budget: at most `max_buckets + EXPIRY_OVERLAP_HOURS` buckets are
/// queried per invocation.
const EXPIRY_OVERLAP_HOURS: u64 = 2;
/// Name of the owner listing index.
pub const OWNER_GSI_NAME: &str = "owner-gsi";
/// Name of the sparse expiry cleanup index.
pub const EXPIRY_GSI_NAME: &str = "expiry-gsi";

/// Deployment environment embedded in an API key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyEnvironment {
    /// Production credentials.
    Live,
    /// Non-production credentials.
    Test,
}

impl KeyEnvironment {
    /// Returns the canonical key segment.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Test => "test",
        }
    }

    fn parse(value: &str) -> Result<Self, ApiKeyError> {
        match value {
            "live" => Ok(Self::Live),
            "test" => Ok(Self::Test),
            _ => Err(ApiKeyError::InvalidEnvironment),
        }
    }
}

/// An external API-key permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ApiKeyScope {
    /// Read external package context.
    ExternalRead,
    /// Request external package indexing.
    ExternalIndex,
    /// Read external indexing status.
    ExternalStatus,
}

impl ApiKeyScope {
    /// Returns the normalized scope name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExternalRead => "external.read",
            Self::ExternalIndex => "external.index",
            Self::ExternalStatus => "external.status",
        }
    }

    fn parse(value: &str) -> Result<Self, ApiKeyError> {
        match value {
            "external.read" => Ok(Self::ExternalRead),
            "external.index" => Ok(Self::ExternalIndex),
            "external.status" => Ok(Self::ExternalStatus),
            _ => Err(ApiKeyError::InvalidScope),
        }
    }
}

/// A sorted, duplicate-free set of permitted external scopes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyScopes(BTreeSet<ApiKeyScope>);

impl ApiKeyScopes {
    /// Parses and normalizes scope names.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is empty or contains an unsupported scope.
    pub fn parse(values: &[&str]) -> Result<Self, ApiKeyError> {
        if values.is_empty() {
            return Err(ApiKeyError::InvalidScope);
        }
        let scopes = values
            .iter()
            .map(|value| ApiKeyScope::parse(value))
            .collect::<Result<BTreeSet<_>, _>>()?;
        Ok(Self(scopes))
    }

    /// Builds a normalized set from typed scopes.
    ///
    /// # Errors
    ///
    /// Returns an error when the resulting set is empty.
    pub fn new(values: impl IntoIterator<Item = ApiKeyScope>) -> Result<Self, ApiKeyError> {
        let scopes = values.into_iter().collect::<BTreeSet<_>>();
        if scopes.is_empty() {
            return Err(ApiKeyError::InvalidScope);
        }
        Ok(Self(scopes))
    }

    /// Returns normalized scope names in stable order.
    #[must_use]
    pub fn as_strings(&self) -> Vec<&'static str> {
        self.0.iter().map(|scope| scope.as_str()).collect()
    }
}

/// Durable lifecycle state for an API key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyStatus {
    /// The key may authenticate until its expiry time.
    Active,
    /// The key has been explicitly revoked.
    Revoked,
}

impl ApiKeyStatus {
    /// Returns the persisted status value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
        }
    }
}

/// Persistable API-key metadata. It intentionally contains no plaintext key.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKeyRecord {
    /// Public lookup identifier.
    pub public_id: String,
    /// Owning principal identifier.
    pub owner_id: String,
    /// Human-readable bounded display name.
    pub name: String,
    /// SHA-256 of the decoded 256-bit secret.
    pub secret_hash: [u8; 32],
    /// Granted external scopes.
    pub scopes: ApiKeyScopes,
    /// Current lifecycle state.
    pub status: ApiKeyStatus,
    /// Creation time as Unix seconds.
    pub created_at: u64,
    /// Expiry time as Unix seconds.
    pub expires_at: u64,
    /// Revocation time as Unix seconds.
    pub revoked_at: Option<u64>,
}

impl fmt::Debug for ApiKeyRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApiKeyRecord")
            .field("public_id", &"[REDACTED]")
            .field("owner_id", &"[REDACTED]")
            .field("name", &"[REDACTED]")
            .field("secret_hash", &"[REDACTED]")
            .field("scopes", &self.scopes)
            .field("status", &self.status)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .field("revoked_at", &self.revoked_at)
            .finish()
    }
}

impl ApiKeyRecord {
    /// Returns whether this record can authenticate at `now_epoch_seconds`.
    #[must_use]
    pub fn is_active_at(&self, now_epoch_seconds: u64) -> bool {
        self.status == ApiKeyStatus::Active && self.expires_at > now_epoch_seconds
    }
}

/// Newly generated key material. The plaintext is returned only here.
pub struct GeneratedApiKey {
    /// Public lookup identifier.
    pub public_id: String,
    /// Full plaintext credential, protected from accidental debug/display.
    pub plaintext: secrecy::SecretString,
    /// Persistable record containing only the secret digest.
    pub record: ApiKeyRecord,
}

impl fmt::Debug for GeneratedApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GeneratedApiKey")
            .field("public_id", &"[REDACTED]")
            .field("plaintext", &"[REDACTED]")
            .field("record", &"[REDACTED]")
            .finish()
    }
}

/// Borrowed segments of a syntactically and canonically valid API key.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ParsedApiKey<'a> {
    /// Environment encoded in the key.
    pub environment: KeyEnvironment,
    /// Canonical public identifier.
    pub public_id: &'a str,
    /// Canonical encoded secret.
    secret: &'a str,
}

impl fmt::Debug for ParsedApiKey<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ParsedApiKey")
            .field("environment", &self.environment)
            .field("public_id", &"[REDACTED]")
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

/// Validation and key-generation failures. Messages never contain key data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ApiKeyError {
    /// Key does not have exactly four canonical segments.
    #[error("invalid API key format")]
    InvalidFormat,
    /// Environment is not `live` or `test`.
    #[error("invalid API key environment")]
    InvalidEnvironment,
    /// A base32 segment is invalid or non-canonical.
    #[error("invalid API key encoding")]
    InvalidEncoding,
    /// Owner identifier is empty or exceeds its bound.
    #[error("invalid API key owner")]
    InvalidOwner,
    /// Name is empty or exceeds its bound.
    #[error("invalid API key name")]
    InvalidName,
    /// Scope is unsupported or the set is empty.
    #[error("invalid API key scope")]
    InvalidScope,
    /// Expiry is not later than creation.
    #[error("invalid API key expiry")]
    InvalidExpiry,
    /// The operating system random source was unavailable.
    #[error("API key generation unavailable")]
    GenerationUnavailable,
}

/// Generates a new API key from OS-provided cryptographically secure random bytes.
///
/// # Errors
///
/// Returns an error for invalid owner/name/expiry values or when the operating
/// system random source is unavailable.
pub fn generate_api_key(
    environment: KeyEnvironment,
    owner_id: &str,
    name: &str,
    scopes: ApiKeyScopes,
    now_epoch_seconds: u64,
    expires_at: u64,
) -> Result<GeneratedApiKey, ApiKeyError> {
    validate_bounded(owner_id, MAX_OWNER_ID_LEN, ApiKeyError::InvalidOwner)?;
    validate_bounded(name, MAX_NAME_LEN, ApiKeyError::InvalidName)?;
    if expires_at <= now_epoch_seconds {
        return Err(ApiKeyError::InvalidExpiry);
    }

    let mut public_id_bytes = [0_u8; PUBLIC_ID_BYTES];
    let mut secret_bytes = [0_u8; SECRET_BYTES];
    OsRng
        .try_fill_bytes(&mut public_id_bytes)
        .map_err(|_| ApiKeyError::GenerationUnavailable)?;
    OsRng
        .try_fill_bytes(&mut secret_bytes)
        .map_err(|_| ApiKeyError::GenerationUnavailable)?;

    let public_id = encode_base32(&public_id_bytes);
    let encoded_secret = encode_base32(&secret_bytes);
    debug_assert_eq!(public_id.len(), PUBLIC_ID_LEN);
    debug_assert_eq!(encoded_secret.len(), SECRET_LEN);
    let plaintext = SecretString::from(format!(
        "spur_{}_{}_{}",
        environment.as_str(),
        public_id,
        encoded_secret
    ));
    let secret_hash = Sha256::digest(secret_bytes).into();
    let record = ApiKeyRecord {
        public_id: public_id.clone(),
        owner_id: owner_id.to_string(),
        name: name.to_string(),
        secret_hash,
        scopes,
        status: ApiKeyStatus::Active,
        created_at: now_epoch_seconds,
        expires_at,
        revoked_at: None,
    };
    Ok(GeneratedApiKey {
        public_id,
        plaintext,
        record,
    })
}

/// Parses and validates an API key without allocating secret material.
///
/// # Errors
///
/// Returns an error when the key grammar, environment, or base32 encoding is invalid.
pub fn parse_api_key(value: &str) -> Result<ParsedApiKey<'_>, ApiKeyError> {
    let mut segments = value.split('_');
    let prefix = segments.next().ok_or(ApiKeyError::InvalidFormat)?;
    let environment = segments.next().ok_or(ApiKeyError::InvalidFormat)?;
    let public_id = segments.next().ok_or(ApiKeyError::InvalidFormat)?;
    let secret = segments.next().ok_or(ApiKeyError::InvalidFormat)?;
    if segments.next().is_some() || prefix != "spur" {
        return Err(ApiKeyError::InvalidFormat);
    }
    if public_id.len() != PUBLIC_ID_LEN || secret.len() != SECRET_LEN {
        return Err(ApiKeyError::InvalidEncoding);
    }
    decode_base32::<PUBLIC_ID_BYTES>(public_id)?;
    decode_base32::<SECRET_BYTES>(secret)?;
    Ok(ParsedApiKey {
        environment: KeyEnvironment::parse(environment)?,
        public_id,
        secret,
    })
}

/// Verifies a parsed secret against a persisted SHA-256 digest in constant time.
#[must_use]
pub fn verify_secret(parsed: &ParsedApiKey<'_>, stored_digest: &[u8]) -> bool {
    let Ok(secret_bytes) = decode_base32::<SECRET_BYTES>(parsed.secret) else {
        return false;
    };
    let digest = Sha256::digest(secret_bytes);
    digest.as_slice().ct_eq(stored_digest).into()
}

fn validate_bounded(value: &str, max_len: usize, error: ApiKeyError) -> Result<(), ApiKeyError> {
    if value.is_empty() || value.len() > max_len || value.trim() != value {
        return Err(error);
    }
    Ok(())
}

fn encode_base32(bytes: &[u8]) -> String {
    let mut output = String::with_capacity((bytes.len() * 8).div_ceil(5));
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            let index = ((buffer >> bits) & 0x1f) as usize;
            output.push(BASE32_ALPHABET[index] as char);
        }
    }
    if bits != 0 {
        let index = ((buffer << (5 - bits)) & 0x1f) as usize;
        output.push(BASE32_ALPHABET[index] as char);
    }
    output
}

fn decode_base32<const N: usize>(value: &str) -> Result<[u8; N], ApiKeyError> {
    let mut output = Vec::with_capacity(N);
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    for byte in value.bytes() {
        let index = u32::try_from(
            BASE32_ALPHABET
                .iter()
                .position(|candidate| *candidate == byte)
                .ok_or(ApiKeyError::InvalidEncoding)?,
        )
        .map_err(|_| ApiKeyError::InvalidEncoding)?;
        buffer = (buffer << 5) | index;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    if output.len() != N || (bits != 0 && buffer & ((1_u32 << bits) - 1) != 0) {
        return Err(ApiKeyError::InvalidEncoding);
    }
    let decoded: [u8; N] = output
        .try_into()
        .map_err(|_| ApiKeyError::InvalidEncoding)?;
    if encode_base32(&decoded) != value {
        return Err(ApiKeyError::InvalidEncoding);
    }
    Ok(decoded)
}

/// Request to atomically persist one generated record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateKeyRecord {
    /// Record containing metadata and a secret digest, never plaintext.
    pub record: ApiKeyRecord,
}

impl CreateKeyRecord {
    /// Wraps a generated record for persistence.
    #[must_use]
    pub fn new(record: ApiKeyRecord) -> Self {
        Self { record }
    }
}

/// One owner-listing page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyPage {
    /// Keys in ascending creation order.
    pub keys: Vec<ApiKeyRecord>,
    /// Opaque continuation cursor, when another page exists.
    pub next_cursor: Option<String>,
}

/// Result of an owner-scoped revoke operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeResult {
    /// The active key was revoked and its owner count decremented.
    Revoked,
    /// The same owner had already revoked the key.
    AlreadyRevoked,
    /// The public ID does not exist for the requested owner.
    NotFound,
}

/// Bounded expiry-sweeper work request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepRequest {
    /// Current Unix time used for expiry-bucket and record-expiry decisions.
    /// Production lease CAS operations use fresh wall-clock reads so a sweep
    /// cannot retain an expired lease by reusing this captured timestamp.
    pub now_epoch_seconds: u64,
    /// First UTC-hour bucket to inspect when no cursor exists.
    pub start_hour: u64,
    /// Maximum number of completed buckets in this call.
    pub max_buckets: usize,
    /// Maximum number of expiry-GSI query pages in this call, including
    /// late-index overlap pages.
    pub max_pages: usize,
    /// Maximum number of expiry records attempted in this call.
    pub max_records: usize,
    /// Maximum records processed from a bucket in this call.
    pub page_limit: usize,
    /// Stable identity of the sweeper holding the lease.
    pub lease_owner: String,
    /// Lease duration in seconds.
    pub lease_duration_seconds: u64,
}

/// Result of a bounded expiry sweep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepPage {
    /// Number of records newly transitioned out of the active state.
    pub processed: usize,
    /// Last fully drained UTC-hour bucket.
    pub completed_hour: Option<u64>,
    /// Whether a partial bucket or another closed bucket remains.
    pub has_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SweepBucketPage {
    scanned: usize,
    processed: usize,
    complete: bool,
}

/// Sanitized durable-store failures.
///
/// Both `Debug` and `Display` contain only bounded variant text, never request
/// data, identifiers, digests, conditions, or provider error details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ApiKeyStoreError {
    /// The supplied record or pagination/sweep request is invalid.
    #[error("invalid API key store request")]
    InvalidRequest,
    /// A record with the public ID already exists.
    #[error("API key public identifier already exists")]
    DuplicatePublicId,
    /// The owner already has ten active keys.
    #[error("API key owner limit reached")]
    OwnerLimit,
    /// Another expiry worker holds the cleanup lease.
    #[error("API key expiry lease is busy")]
    LeaseBusy,
    /// A concurrent conditional write must be retried.
    #[error("API key store conflict")]
    Conflict,
    /// `DynamoDB` or persisted data failed without exposing provider details.
    #[error("API key store unavailable")]
    Backend,
}

/// Storage contract shared by the production and deterministic fake stores.
#[async_trait]
pub trait ApiKeyStore: Send + Sync {
    /// Atomically creates the key and increments its owner's active counter.
    async fn create_key(&self, request: CreateKeyRecord) -> Result<(), ApiKeyStoreError>;

    /// Performs a strongly consistent primary-key authentication lookup.
    ///
    /// Inactive records are returned so the authorizer can distinguish bounded
    /// `expired` and `revoked` decisions from an unknown public identifier.
    async fn get_key_consistent(
        &self,
        public_id: &str,
    ) -> Result<Option<ApiKeyRecord>, ApiKeyStoreError>;

    /// Lists an owner's keys using the owner GSI order.
    async fn list_owner_keys(
        &self,
        owner_id: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<ApiKeyPage, ApiKeyStoreError>;

    /// Revokes an owner-scoped key without double-decrementing the counter.
    async fn revoke_key(
        &self,
        owner_id: &str,
        public_id: &str,
        now: u64,
    ) -> Result<RevokeResult, ApiKeyStoreError>;

    /// Acquires the cleanup lease and drains bounded closed expiry buckets.
    async fn sweep_expired(&self, request: SweepRequest) -> Result<SweepPage, ApiKeyStoreError>;
}

#[derive(Default)]
struct FakeApiKeyState {
    records: HashMap<String, ApiKeyRecord>,
    active_counts: HashMap<String, u64>,
    hidden_expiry_ids: BTreeSet<String>,
    completed_hour: Option<u64>,
    lease_owner: Option<String>,
    lease_token: Option<String>,
    lease_expires_at: Option<u64>,
    cursor_version: u64,
}

impl fmt::Debug for FakeApiKeyState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FakeApiKeyState")
            .field("record_count", &self.records.len())
            .field("active_owner_count", &self.active_counts.len())
            .field("hidden_expiry_count", &self.hidden_expiry_ids.len())
            .field("completed_hour", &self.completed_hour)
            .field("lease_held", &self.lease_token.is_some())
            .field("cursor_version", &self.cursor_version)
            .finish_non_exhaustive()
    }
}

/// Opaque fenced lease used by the fake store's deterministic concurrency model.
#[derive(Clone, PartialEq, Eq)]
pub struct FakeSweepLease {
    token: String,
    version: u64,
    completed_hour: Option<u64>,
    expires_at: u64,
}

impl fmt::Debug for FakeSweepLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FakeSweepLease")
            .field("token", &"[REDACTED]")
            .field("version", &self.version)
            .field("completed_hour", &self.completed_hour)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Deterministic, process-local store mirroring transactional production semantics.
#[derive(Clone, Default)]
pub struct FakeApiKeyStore {
    state: Arc<Mutex<FakeApiKeyState>>,
}

impl fmt::Debug for FakeApiKeyStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.state.lock() {
            Ok(state) => formatter
                .debug_struct("FakeApiKeyStore")
                .field("state", &*state)
                .finish(),
            Err(_) => formatter
                .debug_struct("FakeApiKeyStore")
                .field("state", &"[UNAVAILABLE]")
                .finish(),
        }
    }
}

impl FakeApiKeyStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, FakeApiKeyState>, ApiKeyStoreError> {
        self.state.lock().map_err(|_| ApiKeyStoreError::Backend)
    }

    /// Controls whether a fake record is visible through the eventual expiry
    /// index, allowing deterministic propagation-lag tests.
    ///
    /// # Errors
    ///
    /// Returns an error when the record does not exist or the fake lock is poisoned.
    pub fn set_expiry_index_visible(
        &self,
        public_id: &str,
        visible: bool,
    ) -> Result<(), ApiKeyStoreError> {
        let mut state = self.lock()?;
        if !state.records.contains_key(public_id) {
            return Err(ApiKeyStoreError::InvalidRequest);
        }
        if visible {
            state.hidden_expiry_ids.remove(public_id);
        } else {
            state.hidden_expiry_ids.insert(public_id.to_string());
        }
        Ok(())
    }

    /// Acquires a fake fenced lease only when no unexpired invocation owns it.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid lease values, a held lease, or a poisoned lock.
    pub fn acquire_expiry_lease(
        &self,
        lease_owner: &str,
        token: &str,
        now: u64,
        duration_seconds: u64,
    ) -> Result<FakeSweepLease, ApiKeyStoreError> {
        if lease_owner.is_empty() || token.is_empty() || duration_seconds == 0 {
            return Err(ApiKeyStoreError::InvalidRequest);
        }
        let mut state = self.lock()?;
        if state
            .lease_expires_at
            .is_some_and(|expires_at| expires_at > now)
        {
            return Err(ApiKeyStoreError::LeaseBusy);
        }
        state.cursor_version = state.cursor_version.saturating_add(1);
        state.lease_owner = Some(lease_owner.to_string());
        state.lease_token = Some(token.to_string());
        state.lease_expires_at = Some(now.saturating_add(duration_seconds));
        Ok(FakeSweepLease {
            token: token.to_string(),
            version: state.cursor_version,
            completed_hour: state.completed_hour,
            expires_at: now.saturating_add(duration_seconds),
        })
    }

    /// Conditionally advances the fake cursor with token, version, expected
    /// cursor, lease-expiry, and monotonicity fences.
    ///
    /// # Errors
    ///
    /// Returns an error when a lease fence fails, progression is not monotonic,
    /// or the fake lock is poisoned.
    pub fn save_expiry_cursor(
        &self,
        lease: &mut FakeSweepLease,
        completed_hour: u64,
        now: u64,
    ) -> Result<(), ApiKeyStoreError> {
        let mut state = self.lock()?;
        if state.lease_token.as_deref() != Some(lease.token.as_str())
            || state.cursor_version != lease.version
            || state
                .lease_expires_at
                .is_none_or(|expires_at| expires_at <= now)
        {
            return Err(ApiKeyStoreError::LeaseBusy);
        }
        if state.completed_hour != lease.completed_hour
            || state
                .completed_hour
                .is_some_and(|current| completed_hour <= current)
        {
            return Err(ApiKeyStoreError::Conflict);
        }
        state.completed_hour = Some(completed_hour);
        state.cursor_version = state.cursor_version.saturating_add(1);
        lease.completed_hour = Some(completed_hour);
        lease.version = state.cursor_version;
        Ok(())
    }

    /// Conditionally releases a fake lease. A stale token/version cannot clear
    /// a newer invocation's lease.
    ///
    /// # Errors
    ///
    /// Returns an error when the lease fence fails or the fake lock is poisoned.
    pub fn release_expiry_lease(&self, lease: &FakeSweepLease) -> Result<(), ApiKeyStoreError> {
        let mut state = self.lock()?;
        if state.lease_token.as_deref() != Some(lease.token.as_str())
            || state.cursor_version != lease.version
        {
            return Err(ApiKeyStoreError::LeaseBusy);
        }
        state.lease_owner = None;
        state.lease_token = None;
        state.lease_expires_at = None;
        Ok(())
    }

    /// Returns the fake durable high-water cursor.
    ///
    /// # Errors
    ///
    /// Returns an error when the fake lock is poisoned.
    pub fn expiry_completed_hour(&self) -> Result<Option<u64>, ApiKeyStoreError> {
        Ok(self.lock()?.completed_hour)
    }

    fn process_expiry_bucket(
        &self,
        hour: u64,
        now: u64,
        page_limit: usize,
    ) -> Result<SweepBucketPage, ApiKeyStoreError> {
        let mut state = self.lock()?;
        let mut ids = state
            .records
            .values()
            .filter(|record| {
                record.status == ApiKeyStatus::Active
                    && record.expires_at / 3_600 == hour
                    && record.expires_at <= now
                    && !state.hidden_expiry_ids.contains(&record.public_id)
            })
            .map(|record| (expiry_sort_key(record), record.public_id.clone()))
            .collect::<Vec<_>>();
        ids.sort();
        let complete = ids.len() <= page_limit;
        ids.truncate(page_limit);
        let scanned = ids.len();
        let mut processed = 0;
        for (_, public_id) in ids {
            let owner = {
                let record = state
                    .records
                    .get_mut(&public_id)
                    .ok_or(ApiKeyStoreError::Backend)?;
                if record.status != ApiKeyStatus::Active {
                    continue;
                }
                record.status = ApiKeyStatus::Revoked;
                record.revoked_at = Some(now);
                record.owner_id.clone()
            };
            decrement_active_count(&mut state.active_counts, &owner)?;
            processed += 1;
        }
        Ok(SweepBucketPage {
            scanned,
            processed,
            complete,
        })
    }
}

#[async_trait]
impl ApiKeyStore for FakeApiKeyStore {
    async fn create_key(&self, request: CreateKeyRecord) -> Result<(), ApiKeyStoreError> {
        validate_record(&request.record)?;
        let mut state = self.lock()?;
        if state.records.contains_key(&request.record.public_id) {
            return Err(ApiKeyStoreError::DuplicatePublicId);
        }
        let active_count = state
            .active_counts
            .get(&request.record.owner_id)
            .copied()
            .unwrap_or_default();
        if active_count >= MAX_ACTIVE_KEYS_PER_OWNER {
            return Err(ApiKeyStoreError::OwnerLimit);
        }
        *state
            .active_counts
            .entry(request.record.owner_id.clone())
            .or_default() += 1;
        state
            .records
            .insert(request.record.public_id.clone(), request.record);
        Ok(())
    }

    async fn get_key_consistent(
        &self,
        public_id: &str,
    ) -> Result<Option<ApiKeyRecord>, ApiKeyStoreError> {
        Ok(self.lock()?.records.get(public_id).cloned())
    }

    async fn list_owner_keys(
        &self,
        owner_id: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<ApiKeyPage, ApiKeyStoreError> {
        validate_list_request(owner_id, cursor, limit)?;
        let mut records = self
            .lock()?
            .records
            .values()
            .filter(|record| record.owner_id == owner_id)
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by_key(owner_sort_key);
        if let Some(cursor) = cursor {
            records.retain(|record| owner_sort_key(record).as_str() > cursor);
        }
        let has_more = records.len() > limit;
        records.truncate(limit);
        let next_cursor = has_more.then(|| {
            owner_sort_key(
                records
                    .last()
                    .expect("non-empty page when another page exists"),
            )
        });
        Ok(ApiKeyPage {
            keys: records,
            next_cursor,
        })
    }

    async fn revoke_key(
        &self,
        owner_id: &str,
        public_id: &str,
        now: u64,
    ) -> Result<RevokeResult, ApiKeyStoreError> {
        let mut state = self.lock()?;
        let Some(record) = state.records.get_mut(public_id) else {
            return Ok(RevokeResult::NotFound);
        };
        if record.owner_id != owner_id {
            return Ok(RevokeResult::NotFound);
        }
        if record.status == ApiKeyStatus::Revoked {
            return Ok(RevokeResult::AlreadyRevoked);
        }
        let owner = record.owner_id.clone();
        record.status = ApiKeyStatus::Revoked;
        record.revoked_at = Some(now);
        decrement_active_count(&mut state.active_counts, &owner)?;
        Ok(RevokeResult::Revoked)
    }

    async fn sweep_expired(&self, request: SweepRequest) -> Result<SweepPage, ApiKeyStoreError> {
        validate_sweep_request(&request)?;
        let token = Uuid::new_v4().to_string();
        let mut lease = self.acquire_expiry_lease(
            &request.lease_owner,
            &token,
            request.now_epoch_seconds,
            request.lease_duration_seconds,
        )?;
        let overlap_high_water = lease.completed_hour;
        let Some(through_hour) = last_settled_expiry_hour(request.now_epoch_seconds) else {
            let page = SweepPage {
                processed: 0,
                completed_hour: lease.completed_hour,
                has_more: false,
            };
            self.release_expiry_lease(&lease)?;
            return Ok(page);
        };
        let mut hour = lease
            .completed_hour
            .map_or(request.start_hour, |completed| completed.saturating_add(1));
        let mut processed = 0;
        let mut buckets = 0;
        let mut pages = 0;
        let mut records = 0;
        while hour <= through_hour && buckets < request.max_buckets {
            let Some(page_limit) = sweep_page_limit(&request, pages, records) else {
                break;
            };
            let bucket_page =
                self.process_expiry_bucket(hour, request.now_epoch_seconds, page_limit)?;
            pages += 1;
            records += bucket_page.scanned;
            processed += bucket_page.processed;
            if !bucket_page.complete {
                let page = SweepPage {
                    processed,
                    completed_hour: lease.completed_hour,
                    has_more: true,
                };
                self.release_expiry_lease(&lease)?;
                return Ok(page);
            }
            self.save_expiry_cursor(&mut lease, hour, request.now_epoch_seconds)?;
            hour = hour.saturating_add(1);
            buckets += 1;
        }

        for overlap_hour in overlap_hours(overlap_high_water, request.start_hour) {
            let Some(page_limit) = sweep_page_limit(&request, pages, records) else {
                let page = SweepPage {
                    processed,
                    completed_hour: lease.completed_hour,
                    has_more: true,
                };
                self.release_expiry_lease(&lease)?;
                return Ok(page);
            };
            let bucket_page =
                self.process_expiry_bucket(overlap_hour, request.now_epoch_seconds, page_limit)?;
            pages += 1;
            records += bucket_page.scanned;
            processed += bucket_page.processed;
            if !bucket_page.complete {
                let page = SweepPage {
                    processed,
                    completed_hour: lease.completed_hour,
                    has_more: true,
                };
                self.release_expiry_lease(&lease)?;
                return Ok(page);
            }
        }

        let page = SweepPage {
            processed,
            completed_hour: lease.completed_hour,
            has_more: closed_hour_work_remains(
                lease.completed_hour,
                request.start_hour,
                through_hour,
            ),
        };
        self.release_expiry_lease(&lease)?;
        Ok(page)
    }
}

/// DynamoDB-backed API-key store using a dedicated single-key table.
#[derive(Debug, Clone)]
pub struct DynamoDbApiKeyStore {
    client: DynamoDbClient,
    table_name: String,
    owner_gsi_name: String,
    expiry_gsi_name: String,
}

#[derive(Clone)]
struct DynamoSweepLease {
    token: String,
    version: u64,
    completed_hour: Option<u64>,
}

impl fmt::Debug for DynamoSweepLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DynamoSweepLease")
            .field("token", &"[REDACTED]")
            .field("version", &self.version)
            .field("completed_hour", &self.completed_hour)
            .finish()
    }
}

impl DynamoDbApiKeyStore {
    /// Creates a store using environment configuration and standard GSI names.
    #[must_use]
    pub fn new(client: DynamoDbClient) -> Self {
        Self {
            client,
            table_name: env::var("SPUR_CONTEXT_API_KEYS_TABLE")
                .unwrap_or_else(|_| DEFAULT_API_KEYS_TABLE.to_string()),
            owner_gsi_name: OWNER_GSI_NAME.to_string(),
            expiry_gsi_name: EXPIRY_GSI_NAME.to_string(),
        }
    }

    /// Creates a store for an explicitly named table, useful in local integration tests.
    #[must_use]
    pub fn with_table_name(client: DynamoDbClient, table_name: impl Into<String>) -> Self {
        Self {
            client,
            table_name: table_name.into(),
            owner_gsi_name: OWNER_GSI_NAME.to_string(),
            expiry_gsi_name: EXPIRY_GSI_NAME.to_string(),
        }
    }

    async fn get_record(&self, public_id: &str) -> Result<Option<ApiKeyRecord>, ApiKeyStoreError> {
        let output = self
            .client
            .get_item()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(key_pk(public_id)))
            .consistent_read(true)
            .send()
            .await
            .map_err(|_| ApiKeyStoreError::Backend)?;
        output.item.as_ref().map(record_from_item).transpose()
    }

    async fn acquire_sweep_lease(
        &self,
        request: &SweepRequest,
    ) -> Result<DynamoSweepLease, ApiKeyStoreError> {
        let now = unix_now()?;
        let token = Uuid::new_v4().to_string();
        let result = self
            .client
            .update_item()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(cleanup_cursor_pk().to_string()))
            .update_expression(
                "SET entity = :entity, updated_at = :now, lease_owner = :owner, lease_token = :token, lease_expires_at = :lease_expiry ADD cursor_version :one",
            )
            .condition_expression(
                "attribute_not_exists(lease_expires_at) OR lease_expires_at <= :now",
            )
            .expression_attribute_values(
                ":entity",
                AttributeValue::S("cleanup_cursor".to_string()),
            )
            .expression_attribute_values(
                ":now",
                AttributeValue::N(now.to_string()),
            )
            .expression_attribute_values(
                ":owner",
                AttributeValue::S(request.lease_owner.clone()),
            )
            .expression_attribute_values(
                ":token",
                AttributeValue::S(token.clone()),
            )
            .expression_attribute_values(
                ":lease_expiry",
                AttributeValue::N(
                    now
                        .saturating_add(request.lease_duration_seconds)
                        .to_string(),
                ),
            )
            .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
            .return_values(ReturnValue::AllNew)
            .send()
            .await;
        let output = match result {
            Ok(output) => output,
            Err(error) if update_is_conditional_failure(&error) => {
                return Err(ApiKeyStoreError::LeaseBusy);
            }
            Err(_) => return Err(ApiKeyStoreError::Backend),
        };
        let attributes = output.attributes.ok_or(ApiKeyStoreError::Backend)?;
        Ok(DynamoSweepLease {
            token,
            version: u64_attr(&attributes, "cursor_version")?,
            completed_hour: optional_u64_attr(&attributes, "completed_hour")?,
        })
    }

    async fn save_completed_hour(
        &self,
        lease: &mut DynamoSweepLease,
        hour: u64,
    ) -> Result<(), ApiKeyStoreError> {
        let now = unix_now()?;
        let (condition, expected) = match lease.completed_hour {
            Some(expected) => (
                "lease_token = :token AND cursor_version = :version AND lease_expires_at > :now AND completed_hour = :expected AND completed_hour < :hour",
                Some(expected),
            ),
            None => (
                "lease_token = :token AND cursor_version = :version AND lease_expires_at > :now AND attribute_not_exists(completed_hour)",
                None,
            ),
        };
        let mut request = self
            .client
            .update_item()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(cleanup_cursor_pk().to_string()))
            .update_expression(
                "SET completed_hour = :hour, updated_at = :now ADD cursor_version :one",
            )
            .condition_expression(condition)
            .expression_attribute_values(":hour", AttributeValue::N(hour.to_string()))
            .expression_attribute_values(":now", AttributeValue::N(now.to_string()))
            .expression_attribute_values(":token", AttributeValue::S(lease.token.clone()))
            .expression_attribute_values(":version", AttributeValue::N(lease.version.to_string()))
            .expression_attribute_values(":one", AttributeValue::N("1".to_string()));
        if let Some(expected) = expected {
            request = request
                .expression_attribute_values(":expected", AttributeValue::N(expected.to_string()));
        }
        let result = request.send().await;
        match result {
            Ok(_) => {
                lease.completed_hour = Some(hour);
                lease.version = lease.version.saturating_add(1);
                Ok(())
            }
            Err(error) if update_is_conditional_failure(&error) => Err(ApiKeyStoreError::Conflict),
            Err(_) => Err(ApiKeyStoreError::Backend),
        }
    }

    async fn release_sweep_lease(&self, lease: &DynamoSweepLease) -> Result<(), ApiKeyStoreError> {
        let now = unix_now()?;
        let result = self
            .client
            .update_item()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(cleanup_cursor_pk().to_string()))
            .update_expression(
                "SET updated_at = :now REMOVE lease_owner, lease_token, lease_expires_at",
            )
            .condition_expression("lease_token = :token AND cursor_version = :version")
            .expression_attribute_values(":now", AttributeValue::N(now.to_string()))
            .expression_attribute_values(":token", AttributeValue::S(lease.token.clone()))
            .expression_attribute_values(":version", AttributeValue::N(lease.version.to_string()))
            .send()
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) if update_is_conditional_failure(&error) => Err(ApiKeyStoreError::LeaseBusy),
            Err(_) => Err(ApiKeyStoreError::Backend),
        }
    }

    async fn process_expiry_bucket(
        &self,
        hour: u64,
        now: u64,
        page_limit: usize,
    ) -> Result<SweepBucketPage, ApiKeyStoreError> {
        let query_limit = page_limit.saturating_add(1);
        let output = self
            .client
            .query()
            .table_name(&self.table_name)
            .index_name(&self.expiry_gsi_name)
            .key_condition_expression("expiry_gsi_pk = :bucket")
            .expression_attribute_values(":bucket", AttributeValue::S(expiry_gsi_pk(hour)))
            .limit(i32::try_from(query_limit).map_err(|_| ApiKeyStoreError::InvalidRequest)?)
            .scan_index_forward(true)
            .send()
            .await
            .map_err(|_| ApiKeyStoreError::Backend)?;
        let records = output
            .items
            .unwrap_or_default()
            .iter()
            .map(record_from_item)
            .collect::<Result<Vec<_>, _>>()?;
        let complete = records.len() <= page_limit;
        let scanned = records.len().min(page_limit);
        let mut processed = 0;
        for record in records.into_iter().take(page_limit) {
            if record.expires_at <= now
                && self
                    .revoke_key(&record.owner_id, &record.public_id, now)
                    .await?
                    == RevokeResult::Revoked
            {
                processed += 1;
            }
        }
        Ok(SweepBucketPage {
            scanned,
            processed,
            complete,
        })
    }
}

#[async_trait]
impl ApiKeyStore for DynamoDbApiKeyStore {
    async fn create_key(&self, request: CreateKeyRecord) -> Result<(), ApiKeyStoreError> {
        validate_record(&request.record)?;
        let put = Put::builder()
            .table_name(&self.table_name)
            .set_item(Some(key_item(&request.record)))
            .condition_expression("attribute_not_exists(pk)")
            .build()
            .map_err(|_| ApiKeyStoreError::Backend)?;
        let update = Update::builder()
            .table_name(&self.table_name)
            .key(
                "pk",
                AttributeValue::S(owner_counter_pk(&request.record.owner_id)),
            )
            .update_expression(
                "SET entity = if_not_exists(entity, :entity), active_key_count = if_not_exists(active_key_count, :zero) + :one, updated_at = :now",
            )
            .condition_expression(
                "attribute_not_exists(active_key_count) OR active_key_count < :limit",
            )
            .expression_attribute_values(
                ":entity",
                AttributeValue::S("api_key_owner".to_string()),
            )
            .expression_attribute_values(":zero", AttributeValue::N("0".to_string()))
            .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
            .expression_attribute_values(
                ":limit",
                AttributeValue::N(MAX_ACTIVE_KEYS_PER_OWNER.to_string()),
            )
            .expression_attribute_values(
                ":now",
                AttributeValue::N(request.record.created_at.to_string()),
            )
            .build()
            .map_err(|_| ApiKeyStoreError::Backend)?;
        let result = self
            .client
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().put(put).build())
            .transact_items(TransactWriteItem::builder().update(update).build())
            .send()
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(error) => Err(classify_create_error(&error)),
        }
    }

    async fn get_key_consistent(
        &self,
        public_id: &str,
    ) -> Result<Option<ApiKeyRecord>, ApiKeyStoreError> {
        self.get_record(public_id).await
    }

    async fn list_owner_keys(
        &self,
        owner_id: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<ApiKeyPage, ApiKeyStoreError> {
        validate_list_request(owner_id, cursor, limit)?;
        let exclusive_start_key = cursor.map(|cursor| owner_cursor_key(owner_id, cursor));
        let output = self
            .client
            .query()
            .table_name(&self.table_name)
            .index_name(&self.owner_gsi_name)
            .key_condition_expression("owner_gsi_pk = :owner")
            .expression_attribute_values(":owner", AttributeValue::S(owner_gsi_pk(owner_id)))
            .set_exclusive_start_key(exclusive_start_key)
            .limit(i32::try_from(limit).map_err(|_| ApiKeyStoreError::InvalidRequest)?)
            .scan_index_forward(true)
            .send()
            .await
            .map_err(|_| ApiKeyStoreError::Backend)?;
        let keys = output
            .items
            .unwrap_or_default()
            .iter()
            .map(record_from_item)
            .collect::<Result<Vec<_>, _>>()?;
        let next_cursor = output
            .last_evaluated_key
            .as_ref()
            .map(|key| string_attr(key, "owner_gsi_sk").map(ToOwned::to_owned))
            .transpose()?;
        Ok(ApiKeyPage { keys, next_cursor })
    }

    async fn revoke_key(
        &self,
        owner_id: &str,
        public_id: &str,
        now: u64,
    ) -> Result<RevokeResult, ApiKeyStoreError> {
        let Some(record) = self.get_record(public_id).await? else {
            return Ok(RevokeResult::NotFound);
        };
        if record.owner_id != owner_id {
            return Ok(RevokeResult::NotFound);
        }
        if record.status == ApiKeyStatus::Revoked {
            return Ok(RevokeResult::AlreadyRevoked);
        }
        let key_update = Update::builder()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(key_pk(public_id)))
            .update_expression(
                "SET #status = :revoked, revoked_at = :now REMOVE expiry_gsi_pk, expiry_gsi_sk",
            )
            .condition_expression("owner_id = :owner AND #status = :active")
            .expression_attribute_names("#status", "status")
            .expression_attribute_values(":revoked", AttributeValue::S("revoked".to_string()))
            .expression_attribute_values(":active", AttributeValue::S("active".to_string()))
            .expression_attribute_values(":owner", AttributeValue::S(owner_id.to_string()))
            .expression_attribute_values(":now", AttributeValue::N(now.to_string()))
            .build()
            .map_err(|_| ApiKeyStoreError::Backend)?;
        let owner_update = Update::builder()
            .table_name(&self.table_name)
            .key("pk", AttributeValue::S(owner_counter_pk(owner_id)))
            .update_expression("SET active_key_count = active_key_count - :one, updated_at = :now")
            .condition_expression("active_key_count > :zero")
            .expression_attribute_values(":one", AttributeValue::N("1".to_string()))
            .expression_attribute_values(":zero", AttributeValue::N("0".to_string()))
            .expression_attribute_values(":now", AttributeValue::N(now.to_string()))
            .build()
            .map_err(|_| ApiKeyStoreError::Backend)?;
        let result = self
            .client
            .transact_write_items()
            .transact_items(TransactWriteItem::builder().update(key_update).build())
            .transact_items(TransactWriteItem::builder().update(owner_update).build())
            .send()
            .await;
        match result {
            Ok(_) => Ok(RevokeResult::Revoked),
            Err(error) if transaction_has_conflict(&error) => {
                let current = self.get_record(public_id).await?;
                match current {
                    None => Ok(RevokeResult::NotFound),
                    Some(current) if current.owner_id != owner_id => Ok(RevokeResult::NotFound),
                    Some(current) if current.status == ApiKeyStatus::Revoked => {
                        Ok(RevokeResult::AlreadyRevoked)
                    }
                    Some(_) => Err(ApiKeyStoreError::Conflict),
                }
            }
            Err(_) => Err(ApiKeyStoreError::Backend),
        }
    }

    async fn sweep_expired(&self, request: SweepRequest) -> Result<SweepPage, ApiKeyStoreError> {
        validate_sweep_request(&request)?;
        let mut lease = self.acquire_sweep_lease(&request).await?;
        let overlap_high_water = lease.completed_hour;
        let Some(through_hour) = last_settled_expiry_hour(request.now_epoch_seconds) else {
            let page = SweepPage {
                processed: 0,
                completed_hour: lease.completed_hour,
                has_more: false,
            };
            self.release_sweep_lease(&lease).await?;
            return Ok(page);
        };
        let mut hour = lease
            .completed_hour
            .map_or(request.start_hour, |value| value.saturating_add(1));
        let mut processed = 0;
        let mut buckets = 0;
        let mut pages = 0;
        let mut records = 0;
        while hour <= through_hour && buckets < request.max_buckets {
            let Some(page_limit) = sweep_page_limit(&request, pages, records) else {
                break;
            };
            let bucket_page = self
                .process_expiry_bucket(hour, request.now_epoch_seconds, page_limit)
                .await?;
            pages += 1;
            records += bucket_page.scanned;
            processed += bucket_page.processed;
            if !bucket_page.complete {
                let page = SweepPage {
                    processed,
                    completed_hour: lease.completed_hour,
                    has_more: true,
                };
                self.release_sweep_lease(&lease).await?;
                return Ok(page);
            }
            self.save_completed_hour(&mut lease, hour).await?;
            hour = hour.saturating_add(1);
            buckets += 1;
        }

        for overlap_hour in overlap_hours(overlap_high_water, request.start_hour) {
            let Some(page_limit) = sweep_page_limit(&request, pages, records) else {
                let page = SweepPage {
                    processed,
                    completed_hour: lease.completed_hour,
                    has_more: true,
                };
                self.release_sweep_lease(&lease).await?;
                return Ok(page);
            };
            let bucket_page = self
                .process_expiry_bucket(overlap_hour, request.now_epoch_seconds, page_limit)
                .await?;
            pages += 1;
            records += bucket_page.scanned;
            processed += bucket_page.processed;
            if !bucket_page.complete {
                let page = SweepPage {
                    processed,
                    completed_hour: lease.completed_hour,
                    has_more: true,
                };
                self.release_sweep_lease(&lease).await?;
                return Ok(page);
            }
        }

        let page = SweepPage {
            processed,
            completed_hour: lease.completed_hour,
            has_more: closed_hour_work_remains(
                lease.completed_hour,
                request.start_hour,
                through_hour,
            ),
        };
        self.release_sweep_lease(&lease).await?;
        Ok(page)
    }
}

fn validate_record(record: &ApiKeyRecord) -> Result<(), ApiKeyStoreError> {
    validate_bounded(
        &record.public_id,
        PUBLIC_ID_LEN,
        ApiKeyError::InvalidEncoding,
    )
    .map_err(|_| ApiKeyStoreError::InvalidRequest)?;
    decode_base32::<PUBLIC_ID_BYTES>(&record.public_id)
        .map_err(|_| ApiKeyStoreError::InvalidRequest)?;
    validate_bounded(
        &record.owner_id,
        MAX_OWNER_ID_LEN,
        ApiKeyError::InvalidOwner,
    )
    .map_err(|_| ApiKeyStoreError::InvalidRequest)?;
    validate_bounded(&record.name, MAX_NAME_LEN, ApiKeyError::InvalidName)
        .map_err(|_| ApiKeyStoreError::InvalidRequest)?;
    if record.status != ApiKeyStatus::Active
        || record.revoked_at.is_some()
        || record.expires_at <= record.created_at
    {
        return Err(ApiKeyStoreError::InvalidRequest);
    }
    Ok(())
}

fn validate_list_request(
    owner_id: &str,
    cursor: Option<&str>,
    limit: usize,
) -> Result<(), ApiKeyStoreError> {
    validate_bounded(owner_id, MAX_OWNER_ID_LEN, ApiKeyError::InvalidOwner)
        .map_err(|_| ApiKeyStoreError::InvalidRequest)?;
    if limit == 0
        || limit > MAX_PAGE_LIMIT
        || cursor.is_some_and(|value| parse_cursor(value).is_none())
    {
        return Err(ApiKeyStoreError::InvalidRequest);
    }
    Ok(())
}

fn validate_sweep_request(request: &SweepRequest) -> Result<(), ApiKeyStoreError> {
    validate_bounded(
        &request.lease_owner,
        MAX_OWNER_ID_LEN,
        ApiKeyError::InvalidOwner,
    )
    .map_err(|_| ApiKeyStoreError::InvalidRequest)?;
    if !(1..=MAX_SWEEP_BUCKETS).contains(&request.max_buckets)
        || !(1..=MAX_SWEEP_PAGES).contains(&request.max_pages)
        || !(1..=MAX_SWEEP_RECORDS).contains(&request.max_records)
        || request.page_limit == 0
        || request.page_limit > MAX_PAGE_LIMIT
        || request.lease_duration_seconds == 0
    {
        return Err(ApiKeyStoreError::InvalidRequest);
    }
    Ok(())
}

fn sweep_page_limit(request: &SweepRequest, pages: usize, records: usize) -> Option<usize> {
    if pages >= request.max_pages || records >= request.max_records {
        return None;
    }
    Some(request.page_limit.min(request.max_records - records))
}

fn decrement_active_count(
    active_counts: &mut HashMap<String, u64>,
    owner_id: &str,
) -> Result<(), ApiKeyStoreError> {
    let count = active_counts
        .get_mut(owner_id)
        .ok_or(ApiKeyStoreError::Backend)?;
    *count = count.checked_sub(1).ok_or(ApiKeyStoreError::Backend)?;
    Ok(())
}

fn unix_now() -> Result<u64, ApiKeyStoreError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| ApiKeyStoreError::Backend)
}

fn last_settled_expiry_hour(now_epoch_seconds: u64) -> Option<u64> {
    now_epoch_seconds
        .checked_sub(EXPIRY_GSI_GRACE_SECONDS)
        .map(|settled_time| settled_time / 3_600)
        .and_then(|current_hour| current_hour.checked_sub(1))
}

fn overlap_hours(completed_hour: Option<u64>, start_hour: u64) -> Vec<u64> {
    let Some(completed_hour) = completed_hour else {
        return Vec::new();
    };
    let first = completed_hour
        .saturating_sub(EXPIRY_OVERLAP_HOURS.saturating_sub(1))
        .max(start_hour);
    (first..=completed_hour).collect()
}

fn closed_hour_work_remains(
    completed_hour: Option<u64>,
    start_hour: u64,
    through_hour: u64,
) -> bool {
    completed_hour.map_or(start_hour <= through_hour, |completed| {
        completed
            .checked_add(1)
            .is_some_and(|next_hour| next_hour <= through_hour)
    })
}

fn key_pk(public_id: &str) -> String {
    format!("KEY#{public_id}")
}

fn owner_counter_pk(owner_id: &str) -> String {
    format!("OWNER#{owner_id}")
}

fn owner_gsi_pk(owner_id: &str) -> String {
    format!("OWNER#{owner_id}")
}

fn owner_sort_key(record: &ApiKeyRecord) -> String {
    format!(
        "KEY#{:0width$}#{}",
        record.created_at,
        record.public_id,
        width = SORTABLE_TIMESTAMP_WIDTH
    )
}

fn expiry_gsi_pk(hour: u64) -> String {
    format!("EXPIRY#{hour}")
}

fn expiry_sort_key(record: &ApiKeyRecord) -> String {
    format!(
        "{:0width$}#{}",
        record.expires_at,
        record.public_id,
        width = SORTABLE_TIMESTAMP_WIDTH
    )
}

fn cleanup_cursor_pk() -> &'static str {
    "SYSTEM#expiry-sweeper"
}

fn parse_cursor(cursor: &str) -> Option<(u64, &str)> {
    let remainder = cursor.strip_prefix("KEY#")?;
    let (encoded_created_at, public_id) = remainder.split_once('#')?;
    if encoded_created_at.len() != SORTABLE_TIMESTAMP_WIDTH
        || !encoded_created_at.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let created_at = encoded_created_at.parse().ok()?;
    if format!("{created_at:0SORTABLE_TIMESTAMP_WIDTH$}") != encoded_created_at {
        return None;
    }
    decode_base32::<PUBLIC_ID_BYTES>(public_id).ok()?;
    Some((created_at, public_id))
}

fn owner_cursor_key(owner_id: &str, cursor: &str) -> HashMap<String, AttributeValue> {
    let (_, public_id) = parse_cursor(cursor).expect("cursor validated before key construction");
    HashMap::from([
        ("pk".to_string(), AttributeValue::S(key_pk(public_id))),
        (
            "owner_gsi_pk".to_string(),
            AttributeValue::S(owner_gsi_pk(owner_id)),
        ),
        (
            "owner_gsi_sk".to_string(),
            AttributeValue::S(cursor.to_string()),
        ),
    ])
}

fn key_item(record: &ApiKeyRecord) -> HashMap<String, AttributeValue> {
    let mut item = HashMap::from([
        (
            "pk".to_string(),
            AttributeValue::S(key_pk(&record.public_id)),
        ),
        (
            "entity".to_string(),
            AttributeValue::S("api_key".to_string()),
        ),
        (
            "owner_id".to_string(),
            AttributeValue::S(record.owner_id.clone()),
        ),
        ("name".to_string(), AttributeValue::S(record.name.clone())),
        (
            "secret_hash".to_string(),
            AttributeValue::B(Blob::new(record.secret_hash)),
        ),
        (
            "scopes".to_string(),
            AttributeValue::Ss(
                record
                    .scopes
                    .as_strings()
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            ),
        ),
        (
            "status".to_string(),
            AttributeValue::S(record.status.as_str().to_string()),
        ),
        (
            "created_at".to_string(),
            AttributeValue::N(record.created_at.to_string()),
        ),
        (
            "expires_at".to_string(),
            AttributeValue::N(record.expires_at.to_string()),
        ),
        (
            "ttl".to_string(),
            AttributeValue::N(
                record
                    .expires_at
                    .saturating_add(TTL_GRACE_SECONDS)
                    .to_string(),
            ),
        ),
        (
            "owner_gsi_pk".to_string(),
            AttributeValue::S(owner_gsi_pk(&record.owner_id)),
        ),
        (
            "owner_gsi_sk".to_string(),
            AttributeValue::S(owner_sort_key(record)),
        ),
        (
            "expiry_gsi_pk".to_string(),
            AttributeValue::S(expiry_gsi_pk(record.expires_at / 3_600)),
        ),
        (
            "expiry_gsi_sk".to_string(),
            AttributeValue::S(expiry_sort_key(record)),
        ),
    ]);
    if let Some(revoked_at) = record.revoked_at {
        item.insert(
            "revoked_at".to_string(),
            AttributeValue::N(revoked_at.to_string()),
        );
    }
    item
}

fn record_from_item(
    item: &HashMap<String, AttributeValue>,
) -> Result<ApiKeyRecord, ApiKeyStoreError> {
    if string_attr(item, "entity")? != "api_key" {
        return Err(ApiKeyStoreError::Backend);
    }
    let pk = string_attr(item, "pk")?;
    let public_id = pk.strip_prefix("KEY#").ok_or(ApiKeyStoreError::Backend)?;
    decode_base32::<PUBLIC_ID_BYTES>(public_id).map_err(|_| ApiKeyStoreError::Backend)?;
    let secret_hash = match item.get("secret_hash") {
        Some(AttributeValue::B(value)) => value
            .as_ref()
            .try_into()
            .map_err(|_| ApiKeyStoreError::Backend)?,
        _ => return Err(ApiKeyStoreError::Backend),
    };
    let Some(AttributeValue::Ss(scope_values)) = item.get("scopes") else {
        return Err(ApiKeyStoreError::Backend);
    };
    let scope_refs = scope_values.iter().map(String::as_str).collect::<Vec<_>>();
    let scopes = ApiKeyScopes::parse(&scope_refs).map_err(|_| ApiKeyStoreError::Backend)?;
    let status = match string_attr(item, "status")? {
        "active" => ApiKeyStatus::Active,
        "revoked" => ApiKeyStatus::Revoked,
        _ => return Err(ApiKeyStoreError::Backend),
    };
    Ok(ApiKeyRecord {
        public_id: public_id.to_string(),
        owner_id: string_attr(item, "owner_id")?.to_string(),
        name: string_attr(item, "name")?.to_string(),
        secret_hash,
        scopes,
        status,
        created_at: u64_attr(item, "created_at")?,
        expires_at: u64_attr(item, "expires_at")?,
        revoked_at: optional_u64_attr(item, "revoked_at")?,
    })
}

fn string_attr<'a>(
    item: &'a HashMap<String, AttributeValue>,
    name: &str,
) -> Result<&'a str, ApiKeyStoreError> {
    match item.get(name) {
        Some(AttributeValue::S(value)) => Ok(value),
        _ => Err(ApiKeyStoreError::Backend),
    }
}

fn u64_attr(item: &HashMap<String, AttributeValue>, name: &str) -> Result<u64, ApiKeyStoreError> {
    match item.get(name) {
        Some(AttributeValue::N(value)) => value.parse().map_err(|_| ApiKeyStoreError::Backend),
        _ => Err(ApiKeyStoreError::Backend),
    }
}

fn optional_u64_attr(
    item: &HashMap<String, AttributeValue>,
    name: &str,
) -> Result<Option<u64>, ApiKeyStoreError> {
    match item.get(name) {
        Some(AttributeValue::N(value)) => value
            .parse()
            .map(Some)
            .map_err(|_| ApiKeyStoreError::Backend),
        Some(_) => Err(ApiKeyStoreError::Backend),
        None => Ok(None),
    }
}

fn classify_create_error(error: &SdkError<TransactWriteItemsError>) -> ApiKeyStoreError {
    if transaction_is_in_progress(error) {
        return ApiKeyStoreError::Conflict;
    }
    let Some(reasons) = transaction_cancellation_reasons(error) else {
        return ApiKeyStoreError::Backend;
    };
    classify_create_reasons(reasons)
}

fn classify_create_reasons(
    reasons: &[aws_sdk_dynamodb::types::CancellationReason],
) -> ApiKeyStoreError {
    for (index, reason) in reasons.iter().enumerate() {
        match reason.code() {
            Some("ConditionalCheckFailed") if index == 0 => {
                return ApiKeyStoreError::DuplicatePublicId;
            }
            Some("ConditionalCheckFailed") if index == 1 => {
                return ApiKeyStoreError::OwnerLimit;
            }
            Some("TransactionConflict") => return ApiKeyStoreError::Conflict,
            _ => {}
        }
    }
    ApiKeyStoreError::Backend
}

fn transaction_has_conflict(error: &SdkError<TransactWriteItemsError>) -> bool {
    transaction_is_in_progress(error)
        || transaction_cancellation_reasons(error).is_some_and(|reasons| {
            reasons.iter().any(|reason| {
                matches!(
                    reason.code(),
                    Some("ConditionalCheckFailed" | "TransactionConflict")
                )
            })
        })
}

fn transaction_is_in_progress(error: &SdkError<TransactWriteItemsError>) -> bool {
    error.as_service_error().is_some_and(|error| {
        matches!(
            error,
            TransactWriteItemsError::TransactionInProgressException(_)
        )
    })
}

fn transaction_cancellation_reasons(
    error: &SdkError<TransactWriteItemsError>,
) -> Option<&[aws_sdk_dynamodb::types::CancellationReason]> {
    let SdkError::ServiceError(service_error) = error else {
        return None;
    };
    let TransactWriteItemsError::TransactionCanceledException(inner) = service_error.err() else {
        return None;
    };
    Some(inner.cancellation_reasons())
}

fn update_is_conditional_failure(error: &SdkError<UpdateItemError>) -> bool {
    error
        .as_service_error()
        .is_some_and(UpdateItemError::is_conditional_check_failed_exception)
}

#[cfg(test)]
mod tests {
    use aws_sdk_dynamodb::types::CancellationReason;

    use super::{
        classify_create_reasons, expiry_sort_key, owner_sort_key, ApiKeyRecord, ApiKeyScopes,
        ApiKeyStatus, ApiKeyStoreError,
    };

    fn sort_key_record(created_at: u64, expires_at: u64) -> ApiKeyRecord {
        ApiKeyRecord {
            public_id: "aaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
            owner_id: "owner".to_string(),
            name: "key".to_string(),
            secret_hash: [0; 32],
            scopes: ApiKeyScopes::parse(&["external.read"]).expect("valid scope"),
            status: ApiKeyStatus::Active,
            created_at,
            expires_at,
            revoked_at: None,
        }
    }

    #[test]
    fn timestamp_sort_keys_are_fixed_width_and_lexically_ordered() {
        let records = [
            sort_key_record(9, 9),
            sort_key_record(10, 10),
            sort_key_record(99, 99),
            sort_key_record(100, 100),
        ];

        let owner_keys = records.iter().map(owner_sort_key).collect::<Vec<_>>();
        assert_eq!(
            owner_keys,
            [
                "KEY#00000000000000000009#aaaaaaaaaaaaaaaaaaaaaaaaaa",
                "KEY#00000000000000000010#aaaaaaaaaaaaaaaaaaaaaaaaaa",
                "KEY#00000000000000000099#aaaaaaaaaaaaaaaaaaaaaaaaaa",
                "KEY#00000000000000000100#aaaaaaaaaaaaaaaaaaaaaaaaaa",
            ]
        );
        assert!(owner_keys.windows(2).all(|keys| keys[0] < keys[1]));

        let expiry_keys = records.iter().map(expiry_sort_key).collect::<Vec<_>>();
        assert_eq!(
            expiry_keys,
            [
                "00000000000000000009#aaaaaaaaaaaaaaaaaaaaaaaaaa",
                "00000000000000000010#aaaaaaaaaaaaaaaaaaaaaaaaaa",
                "00000000000000000099#aaaaaaaaaaaaaaaaaaaaaaaaaa",
                "00000000000000000100#aaaaaaaaaaaaaaaaaaaaaaaaaa",
            ]
        );
        assert!(expiry_keys.windows(2).all(|keys| keys[0] < keys[1]));
    }

    #[test]
    fn create_cancellation_positions_map_to_bounded_errors() {
        let duplicate = [
            CancellationReason::builder()
                .code("ConditionalCheckFailed")
                .build(),
            CancellationReason::builder().code("None").build(),
        ];
        assert_eq!(
            classify_create_reasons(&duplicate),
            ApiKeyStoreError::DuplicatePublicId
        );

        let owner_full = [
            CancellationReason::builder().code("None").build(),
            CancellationReason::builder()
                .code("ConditionalCheckFailed")
                .build(),
        ];
        assert_eq!(
            classify_create_reasons(&owner_full),
            ApiKeyStoreError::OwnerLimit
        );

        let contention = [CancellationReason::builder()
            .code("TransactionConflict")
            .build()];
        assert_eq!(
            classify_create_reasons(&contention),
            ApiKeyStoreError::Conflict
        );
    }
}
