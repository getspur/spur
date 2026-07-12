//! Context-service authentication, personal-key lifecycle, and MCP launch.

use std::io::{IsTerminal as _, Read as _};
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use clap::Subcommand;
use secrecy::{ExposeSecret as _, SecretString};
use spur_acp::config::{ContextServiceAuthMode, ContextServiceConfig};
use spur_context_auth::credentials::{
    resolve_api_key, resolve_management, ApiKeyCredential, CredentialProfile, CredentialPurpose,
    CredentialStore, ManagementCredential, OsKeyringCredentialStore, RestrictedFileCredentialStore,
    StoredCredential,
};
use spur_context_auth::management::{CreateApiKeyRequest, ManagementClient, ManagementError};
use spur_context_auth::oauth::{DiscoveryDocument, HumanClient, HumanConfig, ManagementSession};
use spur_core::mcp::ContextServiceAuth;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use url::Url;

use super::{config_set, mcp};

const DEFAULT_PROFILE: &str = "default";
const KEYRING_SERVICE: &str = "dev.getspur.spur.context-service";
const API_KEY_ENV: &str = "SPUR_CONTEXT_SERVICE_API_KEY";
const URL_ENV: &str = "SPUR_CONTEXT_SERVICE_URL";
const LEGACY_TOKEN_ENV: &str = "SPUR_CONTEXT_SERVICE_TOKEN";
const CREDENTIALS_FILE_ENV: &str = "SPUR_CONTEXT_CREDENTIALS_FILE";
const MANAGEMENT_SCOPE: &str = "urn:spur:context-service/keys.manage";
const OAUTH_CALLBACK_URL: &str = spur_context_auth::oauth::HUMAN_CALLBACK_URL;
const OAUTH_CALLBACK_ADDR: &str = "127.0.0.1:8765";
const OAUTH_CALLBACK_PORT: u16 = 8765;
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);
const COMPENSATING_REVOKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CALLBACK_BYTES: usize = 8 * 1024;

enum ResolvedMcpAuth {
    Explicit(ContextServiceAuth),
    LegacyBearer(SecretString),
}

struct StoredCreatedKey {
    key_id: String,
    revealed: Option<String>,
}

#[derive(Subcommand)]
pub enum ContextCommands {
    /// Authenticate a human for OAuth-only API-key management.
    Auth {
        #[command(subcommand)]
        command: ContextAuthCommands,
    },
    /// Create, list, select, revoke, or import personal API keys.
    Key {
        #[command(subcommand)]
        command: ContextKeyCommands,
    },
    /// Run the external code-context MCP server over stdio.
    Mcp {
        /// Context-service origin. Falls back to env, config, then the built-in default.
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        /// Credential profile override.
        #[arg(long, value_name = "NAME")]
        profile: Option<String>,
        /// Deprecated bearer compatibility option. Prefer a stored API key.
        #[arg(long, value_name = "TOKEN")]
        token: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum ContextAuthCommands {
    /// Sign in through Cognito authorization code plus PKCE.
    Login {
        /// Management credential profile.
        #[arg(long, default_value = DEFAULT_PROFILE)]
        profile: String,
        /// Context-service origin override.
        #[arg(long, value_name = "URL")]
        url: Option<String>,
    },
    /// Delete OAuth management credentials without deleting API keys.
    Logout {
        /// Management credential profile.
        #[arg(long, default_value = DEFAULT_PROFILE)]
        profile: String,
    },
}

#[derive(Subcommand)]
pub enum ContextKeyCommands {
    /// Create and immediately store a personal API key.
    Create {
        #[arg(long)]
        name: String,
        #[arg(long = "scope", required = true)]
        scopes: Vec<String>,
        #[arg(long)]
        expires_at: Option<u64>,
        /// Management credential profile.
        #[arg(long, default_value = DEFAULT_PROFILE)]
        profile: String,
        /// Print the one-time key only when stdout is an interactive terminal.
        #[arg(long)]
        show_secret: bool,
    },
    /// List secret-free metadata for the signed-in human.
    List {
        #[arg(long, default_value = DEFAULT_PROFILE)]
        profile: String,
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Select a locally stored key by public key ID.
    Use { key_id: String },
    /// Revoke one personal key through OAuth management authentication.
    Revoke {
        key_id: String,
        #[arg(long, default_value = DEFAULT_PROFILE)]
        profile: String,
    },
    /// Import exactly one canonical API key from stdin.
    Add {
        #[arg(long, required = true)]
        stdin: bool,
        /// Local storage profile. Defaults to the key's public ID.
        #[arg(long)]
        profile: Option<String>,
    },
}

pub async fn run(repo_root: &Path, command: ContextCommands) -> Result<()> {
    let config = spur_acp::config::load_layered(repo_root)?;
    match command {
        ContextCommands::Auth { command } => {
            run_auth(repo_root, &config.context_service, command).await
        }
        ContextCommands::Key { command } => {
            run_key(repo_root, &config.context_service, command).await
        }
        ContextCommands::Mcp {
            url,
            profile,
            token,
        } => {
            let (url, auth) =
                resolve_mcp_auth(&config.context_service, url, profile, token).await?;
            match auth {
                ResolvedMcpAuth::Explicit(auth) => mcp::run_context_server(url, auth).await,
                ResolvedMcpAuth::LegacyBearer(token) => {
                    mcp::run_legacy_context_server(url, token.expose_secret().to_owned()).await
                }
            }
        }
    }
}

async fn run_auth(
    repo_root: &Path,
    config: &ContextServiceConfig,
    command: ContextAuthCommands,
) -> Result<()> {
    match command {
        ContextAuthCommands::Login { profile, url } => {
            let profile_name = profile.clone();
            let service_url = service_url(config, url)?;
            let discovery = DiscoveryDocument::fetch(&service_url)
                .await
                .map_err(|_error| anyhow!("could not load context-service OAuth discovery"))?;
            if !discovery.api_key_auth_enabled() {
                bail!("context-service API-key authentication is disabled");
            }
            let listener = bind_oauth_callback().await?;
            let client = HumanClient::new(HumanConfig::from_discovery(&discovery)?)?;
            let mut pending = client.begin_authorization(["openid", MANAGEMENT_SCOPE])?;
            open_browser(pending.authorization_url())?;
            eprintln!("Waiting for context-service sign-in in your browser...");
            let callback_url = accept_callback(&listener, OAUTH_CALLBACK_PORT).await?;
            let callback = pending.parse_callback(&callback_url)?;
            let session = client.finish_authorization(&mut pending, callback).await?;
            let stored = StoredCredential::Management(credential_from_session(&session)?);
            let profile = CredentialProfile::new(profile, CredentialPurpose::Management)?;
            store_credential(&profile, &stored).await?;
            config_set::set_context_service_selection(
                repo_root,
                service_url.as_str().trim_end_matches('/'),
                ContextServiceAuthMode::OAuthBearer,
                &profile_name,
                None,
            )?;
            println!("Context-service management login stored.");
            Ok(())
        }
        ContextAuthCommands::Logout { profile } => {
            let profile = CredentialProfile::new(profile, CredentialPurpose::Management)?;
            delete_credential(&profile).await?;
            println!("Context-service management login removed; API keys were preserved.");
            Ok(())
        }
    }
}

async fn run_key(
    repo_root: &Path,
    config: &ContextServiceConfig,
    command: ContextKeyCommands,
) -> Result<()> {
    match command {
        ContextKeyCommands::Create {
            name,
            scopes,
            expires_at,
            profile,
            show_secret,
        } => {
            if show_secret && !std::io::stdout().is_terminal() {
                bail!("--show-secret requires stdout to be an interactive terminal");
            }
            let (client, management_profile) = management_client(config, &profile).await?;
            let created = create_and_store_key(
                &client,
                &management_profile,
                CreateApiKeyRequest::new(name, scopes, expires_at)?,
                show_secret,
            )
            .await?;
            let key_id = created.key_id;
            let service_url = service_url(config, None)?;
            config_set::set_context_service_selection(
                repo_root,
                service_url.as_str().trim_end_matches('/'),
                ContextServiceAuthMode::ApiKey,
                &key_id,
                Some(&key_id),
            )?;
            println!("Created and selected context API key {key_id}.");
            if let Some(secret) = created.revealed {
                println!("{secret}");
            }
            Ok(())
        }
        ContextKeyCommands::List { profile, cursor } => {
            let (client, management_profile) = management_client(config, &profile).await?;
            let operation = client.list_keys(cursor.as_deref(), Some(100)).await;
            let page = finish_management_attempt(&client, &management_profile, operation).await?;
            for key in page.keys() {
                println!("{}\t{}", key.key_id(), key.status().as_str());
            }
            if let Some(cursor) = page.next_cursor() {
                println!("next_cursor\t{cursor}");
            }
            Ok(())
        }
        ContextKeyCommands::Use { key_id } => {
            let profile = CredentialProfile::new(&key_id, CredentialPurpose::ApiKey)?;
            let key = resolve_stored_api_key(&profile, None).await?;
            let Some(key) = key else {
                bail!("context API-key profile was not found locally");
            };
            if key.public_id() != key_id {
                bail!("context API-key profile does not match the requested public ID");
            }
            let service_url = service_url(config, None)?;
            config_set::set_context_service_selection(
                repo_root,
                service_url.as_str().trim_end_matches('/'),
                ContextServiceAuthMode::ApiKey,
                &key_id,
                Some(&key_id),
            )?;
            println!("Selected local context API key {key_id}.");
            Ok(())
        }
        ContextKeyCommands::Revoke { key_id, profile } => {
            let (client, management_profile) = management_client(config, &profile).await?;
            let operation = client.revoke_key(&key_id).await;
            let revoked =
                finish_management_attempt(&client, &management_profile, operation).await?;
            println!("{}\t{}", key_id, revoked.status().as_str());
            Ok(())
        }
        ContextKeyCommands::Add { stdin: _, profile } => {
            let mut input = String::new();
            std::io::stdin()
                .take(512)
                .read_to_string(&mut input)
                .context("read context API key from stdin")?;
            let key = ApiKeyCredential::parse_stdin(&input)
                .map_err(|_error| anyhow!("stdin did not contain one canonical context API key"))?;
            let public_id = key.public_id().to_owned();
            let profile_name = profile.unwrap_or_else(|| public_id.clone());
            let profile = CredentialProfile::new(&profile_name, CredentialPurpose::ApiKey)?;
            store_credential(&profile, &StoredCredential::ApiKey(key)).await?;
            let service_url = service_url(config, None)?;
            config_set::set_context_service_selection(
                repo_root,
                service_url.as_str().trim_end_matches('/'),
                ContextServiceAuthMode::ApiKey,
                &profile_name,
                Some(&public_id),
            )?;
            println!("Imported context API key {public_id} into profile {profile_name}.");
            Ok(())
        }
    }
}

async fn resolve_mcp_auth(
    config: &ContextServiceConfig,
    url_override: Option<String>,
    profile_override: Option<String>,
    token_override: Option<String>,
) -> Result<(String, ResolvedMcpAuth)> {
    let url = service_url(config, url_override)?.to_string();
    if let Some(token) = token_override.and_then(non_empty) {
        eprintln!(
            "warning: --token is deprecated; use `spur context key add --stdin` and API-key auth"
        );
        return Ok((url, ResolvedMcpAuth::LegacyBearer(token.into())));
    }

    let environment_key = std::env::var(API_KEY_ENV).ok().and_then(non_empty);
    let (mode, profile_name) =
        mcp_mode_and_profile(config, profile_override, environment_key.is_some());
    match mode {
        ContextServiceAuthMode::None => {
            let legacy = config
                .token
                .clone()
                .and_then(non_empty)
                .or_else(|| std::env::var(LEGACY_TOKEN_ENV).ok().and_then(non_empty));
            match legacy {
                Some(token) => {
                    eprintln!(
                        "warning: legacy context bearer configuration is deprecated; use a stored API key"
                    );
                    Ok((url, ResolvedMcpAuth::LegacyBearer(token.into())))
                }
                None => Ok((url, ResolvedMcpAuth::Explicit(ContextServiceAuth::None))),
            }
        }
        ContextServiceAuthMode::ApiKey => {
            let profile = CredentialProfile::new(profile_name, CredentialPurpose::ApiKey)?;
            let key = resolve_stored_api_key(&profile, environment_key.as_deref())
                .await?
                .ok_or_else(|| {
                    anyhow!("no context API key found; run `spur context key add --stdin`")
                })?;
            Ok((
                url,
                ResolvedMcpAuth::Explicit(ContextServiceAuth::ApiKey(
                    key.secret().expose_secret().to_owned().into(),
                )),
            ))
        }
        ContextServiceAuthMode::OAuthBearer => {
            let profile = CredentialProfile::new(profile_name, CredentialPurpose::Management)?;
            let credential = resolve_stored_management(&profile).await?.ok_or_else(|| {
                anyhow!("no management login found; run `spur context auth login`")
            })?;
            let session = credential.session()?;
            Ok((
                url,
                ResolvedMcpAuth::Explicit(ContextServiceAuth::OAuthBearer(
                    session.access_token().expose_secret().to_owned().into(),
                )),
            ))
        }
    }
}

fn mcp_mode_and_profile(
    config: &ContextServiceConfig,
    profile_override: Option<String>,
    has_environment_api_key: bool,
) -> (ContextServiceAuthMode, String) {
    let mode = if has_environment_api_key {
        ContextServiceAuthMode::ApiKey
    } else {
        config.auth_mode
    };
    let profile = profile_override.unwrap_or_else(|| config.profile.clone());
    (mode, profile)
}

async fn management_client(
    config: &ContextServiceConfig,
    profile_name: &str,
) -> Result<(ManagementClient, CredentialProfile)> {
    let profile = CredentialProfile::new(profile_name, CredentialPurpose::Management)?;
    let credential = resolve_stored_management(&profile)
        .await?
        .ok_or_else(|| anyhow!("no management login found; run `spur context auth login`"))?;
    let service_url = service_url(config, None)?;
    let discovery = DiscoveryDocument::fetch(&service_url)
        .await
        .map_err(|_error| anyhow!("could not load context-service OAuth discovery"))?;
    if !discovery.api_key_auth_enabled() {
        bail!("context-service API-key authentication is disabled");
    }
    let client = ManagementClient::new(discovery, credential.session()?)?;
    Ok((client, profile))
}

async fn create_and_store_key(
    client: &ManagementClient,
    management_profile: &CredentialProfile,
    request: CreateApiKeyRequest,
    reveal: bool,
) -> Result<StoredCreatedKey> {
    if let Some(file) = restricted_file() {
        create_and_store_key_with_stores(client, management_profile, &file, &file, request, reveal)
            .await
    } else {
        let keyring = keyring()?;
        create_and_store_key_with_stores(
            client,
            management_profile,
            &keyring,
            &keyring,
            request,
            reveal,
        )
        .await
    }
}

async fn create_and_store_key_with_stores(
    client: &ManagementClient,
    management_profile: &CredentialProfile,
    management_store: &dyn CredentialStore,
    api_store: &dyn CredentialStore,
    request: CreateApiKeyRequest,
    reveal: bool,
) -> Result<StoredCreatedKey> {
    let created = match client.create_key(request).await {
        Ok(created) => created,
        Err(error) => {
            let _ =
                persist_management_session_with_store(client, management_profile, management_store)
                    .await;
            return Err(anyhow!(
                "context-service management request failed: {error}"
            ));
        }
    };
    let key_id = created.key_id().to_owned();
    if persist_management_session_with_store(client, management_profile, management_store)
        .await
        .is_err()
    {
        return Err(
            compensate_created_key(client, management_profile, management_store, &key_id).await,
        );
    }
    let revealed = reveal.then(|| created.key().expose_secret().to_owned());
    let api_profile = match CredentialProfile::new(&key_id, CredentialPurpose::ApiKey) {
        Ok(profile) => profile,
        Err(_error) => {
            return Err(compensate_created_key(
                client,
                management_profile,
                management_store,
                &key_id,
            )
            .await);
        }
    };
    if api_store
        .store(
            &api_profile,
            &StoredCredential::ApiKey(created.into_credential()),
        )
        .await
        .is_err()
    {
        return Err(
            compensate_created_key(client, management_profile, management_store, &key_id).await,
        );
    }
    Ok(StoredCreatedKey { key_id, revealed })
}

async fn compensate_created_key(
    client: &ManagementClient,
    management_profile: &CredentialProfile,
    management_store: &dyn CredentialStore,
    key_id: &str,
) -> anyhow::Error {
    let recovery =
        tokio::time::timeout(COMPENSATING_REVOKE_TIMEOUT, client.revoke_key(key_id)).await;
    let _ =
        persist_management_session_with_store(client, management_profile, management_store).await;
    match recovery {
        Ok(Ok(_)) => anyhow!(
            "context API key was created but local credential storage failed; the new key was revoked"
        ),
        Ok(Err(_)) | Err(_) => anyhow!(
            "context API key {key_id} was created but local credential storage failed and automatic revocation failed; revoke it manually"
        ),
    }
}

async fn persist_management_session_with_store(
    client: &ManagementClient,
    profile: &CredentialProfile,
    store: &dyn CredentialStore,
) -> Result<()> {
    let session = client.session().await;
    store
        .store(
            profile,
            &StoredCredential::Management(credential_from_session(&session)?),
        )
        .await?;
    Ok(())
}

async fn finish_management_attempt<T>(
    client: &ManagementClient,
    profile: &CredentialProfile,
    attempt: Result<T, ManagementError>,
) -> Result<T> {
    if let Some(file) = restricted_file() {
        finish_management_attempt_with_store(client, profile, &file, attempt).await
    } else {
        let keyring = keyring()?;
        finish_management_attempt_with_store(client, profile, &keyring, attempt).await
    }
}

async fn finish_management_attempt_with_store<T>(
    client: &ManagementClient,
    profile: &CredentialProfile,
    store: &dyn CredentialStore,
    attempt: Result<T, ManagementError>,
) -> Result<T> {
    let persistence = persist_management_session_with_store(client, profile, store).await;
    match attempt {
        Err(error) => {
            let _ = persistence;
            Err(anyhow!(
                "context-service management request failed: {error}"
            ))
        }
        Ok(value) => {
            persistence?;
            Ok(value)
        }
    }
}

fn credential_from_session(session: &ManagementSession) -> Result<ManagementCredential> {
    Ok(ManagementCredential::new(
        session.access_token().expose_secret(),
        session.refresh_token().expose_secret(),
        session.expires_at(),
        session.issuer().as_str(),
        session.client_id(),
    )?)
}

fn keyring() -> Result<OsKeyringCredentialStore> {
    Ok(OsKeyringCredentialStore::new(KEYRING_SERVICE)?)
}

fn restricted_file() -> Option<RestrictedFileCredentialStore> {
    std::env::var_os(CREDENTIALS_FILE_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(RestrictedFileCredentialStore::new)
}

async fn store_credential(profile: &CredentialProfile, value: &StoredCredential) -> Result<()> {
    if let Some(file) = restricted_file() {
        file.store(profile, value).await?;
    } else {
        keyring()?.store(profile, value).await?;
    }
    Ok(())
}

async fn delete_credential(profile: &CredentialProfile) -> Result<()> {
    if let Some(file) = restricted_file() {
        file.delete(profile).await?;
    } else {
        keyring()?.delete(profile).await?;
    }
    Ok(())
}

async fn resolve_stored_api_key(
    profile: &CredentialProfile,
    environment: Option<&str>,
) -> Result<Option<ApiKeyCredential>> {
    let keyring = keyring()?;
    let file = restricted_file();
    resolve_api_key_with_stores(
        profile,
        environment,
        &keyring,
        file.as_ref().map(|store| store as &dyn CredentialStore),
    )
    .await
}

async fn resolve_api_key_with_stores(
    profile: &CredentialProfile,
    environment: Option<&str>,
    keyring: &dyn CredentialStore,
    selected_file: Option<&dyn CredentialStore>,
) -> Result<Option<ApiKeyCredential>> {
    let selected = selected_file.unwrap_or(keyring);
    Ok(resolve_api_key(profile, environment, selected, None).await?)
}

async fn resolve_stored_management(
    profile: &CredentialProfile,
) -> Result<Option<ManagementCredential>> {
    let keyring = keyring()?;
    let file = restricted_file();
    let selected = file
        .as_ref()
        .map_or(&keyring as &dyn CredentialStore, |store| {
            store as &dyn CredentialStore
        });
    Ok(resolve_management(profile, selected, None).await?)
}

fn service_url(config: &ContextServiceConfig, override_url: Option<String>) -> Result<Url> {
    service_url_with_environment(config, override_url, std::env::var(URL_ENV).ok())
}

fn service_url_with_environment(
    config: &ContextServiceConfig,
    override_url: Option<String>,
    environment_url: Option<String>,
) -> Result<Url> {
    let value = override_url
        .and_then(non_empty)
        .or_else(|| environment_url.and_then(non_empty))
        .or_else(|| non_empty(config.url.clone()))
        .unwrap_or_else(|| ContextServiceConfig::default().url);
    Url::parse(&value).map_err(|_error| anyhow!("invalid context-service URL"))
}

fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(target_os = "macos")]
fn open_browser(url: &Url) -> Result<()> {
    let mut command = Command::new("open");
    spawn_browser(&mut command, url)
}

#[cfg(target_os = "linux")]
fn open_browser(url: &Url) -> Result<()> {
    let mut command = Command::new("xdg-open");
    spawn_browser(&mut command, url)
}

#[cfg(target_os = "windows")]
fn open_browser(url: &Url) -> Result<()> {
    windows_browser_command(url)
        .spawn()
        .map(|_| ())
        .map_err(|_error| anyhow!("could not open the system browser for context-service login"))
}

#[cfg(any(test, target_os = "windows"))]
fn windows_browser_command(url: &Url) -> Command {
    let mut command = Command::new("rundll32.exe");
    command.arg("url.dll,FileProtocolHandler").arg(url.as_str());
    command
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn open_browser(_url: &Url) -> Result<()> {
    Err(anyhow!(
        "automatic browser launch is unsupported on this platform"
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn spawn_browser(command: &mut Command, url: &Url) -> Result<()> {
    command
        .arg(url.as_str())
        .spawn()
        .map(|_| ())
        .map_err(|_error| anyhow!("could not open the system browser for context-service login"))
}

async fn accept_callback(listener: &tokio::net::TcpListener, port: u16) -> Result<Url> {
    let (mut stream, _) = tokio::time::timeout(CALLBACK_TIMEOUT, listener.accept())
        .await
        .map_err(|_error| anyhow!("context-service login callback timed out"))?
        .map_err(|_error| anyhow!("context-service login callback failed"))?;
    let mut request = Vec::new();
    loop {
        if request.len() >= MAX_CALLBACK_BYTES {
            bail!("context-service login callback was too large");
        }
        let mut chunk = [0_u8; 1024];
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|_error| anyhow!("context-service login callback failed"))?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let request = std::str::from_utf8(&request)
        .map_err(|_error| anyhow!("context-service login callback was invalid"))?;
    let first_line = request
        .split("\r\n")
        .next()
        .ok_or_else(|| anyhow!("context-service login callback was invalid"))?;
    let mut fields = first_line.split_ascii_whitespace();
    let valid_method = fields.next() == Some("GET");
    let target = fields.next().unwrap_or_default();
    let valid_version = matches!(fields.next(), Some("HTTP/1.1" | "HTTP/1.0"));
    if !valid_method
        || !valid_version
        || fields.next().is_some()
        || !target.starts_with("/callback?")
    {
        bail!("context-service login callback was rejected");
    }
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 37\r\nConnection: close\r\n\r\nSPUR sign-in complete. You may close.",
        )
        .await
        .map_err(|_error| anyhow!("context-service login callback failed"))?;
    Url::parse(&format!("http://127.0.0.1:{port}{target}"))
        .map_err(|_error| anyhow!("context-service login callback was invalid"))
}

async fn bind_oauth_callback() -> Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(OAUTH_CALLBACK_ADDR)
        .await
        .map_err(|_error| {
            anyhow!(
                "could not bind the registered OAuth callback {OAUTH_CALLBACK_URL}; make sure port 8765 is available"
            )
        })
}

#[cfg(test)]
mod context_service_cli_config_tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;
    use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

    #[test]
    fn context_cli_resolution_prefers_flag_url() {
        let config = ContextServiceConfig::default();
        let url = service_url(&config, Some(" https://override.example.test ".to_owned()))
            .expect("valid override");
        assert_eq!(url.as_str(), "https://override.example.test/");
    }

    #[test]
    fn context_cli_resolution_uses_non_secret_defaults() {
        let config = ContextServiceConfig::default();
        assert_eq!(config.auth_mode, ContextServiceAuthMode::None);
        assert_eq!(config.profile, DEFAULT_PROFILE);
        assert!(config.token.is_none());
    }

    #[test]
    fn context_service_url_precedence_is_flag_env_config_default() {
        assert_eq!(
            ContextServiceConfig::default().url,
            "https://context.getspur.dev"
        );

        let mut config = ContextServiceConfig {
            url: "https://config.example.test".to_owned(),
            ..ContextServiceConfig::default()
        };

        assert_eq!(
            service_url_with_environment(
                &config,
                Some("https://flag.example.test".to_owned()),
                Some("https://env.example.test".to_owned()),
            )
            .expect("flag URL")
            .as_str(),
            "https://flag.example.test/"
        );
        assert_eq!(
            service_url_with_environment(
                &config,
                None,
                Some("https://env.example.test".to_owned()),
            )
            .expect("environment URL")
            .as_str(),
            "https://env.example.test/"
        );
        assert_eq!(
            service_url_with_environment(&config, None, None)
                .expect("config URL")
                .as_str(),
            "https://config.example.test/"
        );
        config.url.clear();
        assert_eq!(
            service_url_with_environment(&config, None, None)
                .expect("default URL")
                .as_str(),
            ContextServiceConfig::default().url + "/"
        );
    }

    #[test]
    fn profile_override_preserves_configured_oauth_mode() {
        let config = ContextServiceConfig {
            auth_mode: ContextServiceAuthMode::OAuthBearer,
            profile: "primary".to_owned(),
            ..ContextServiceConfig::default()
        };

        let (mode, profile) = mcp_mode_and_profile(&config, Some("secondary".to_owned()), false);

        assert_eq!(mode, ContextServiceAuthMode::OAuthBearer);
        assert_eq!(profile, "secondary");
    }

    #[test]
    fn windows_browser_launcher_does_not_use_a_command_shell() {
        let url =
            Url::parse("https://login.example.test/authorize?a=1&b=2").expect("authorization URL");
        let command = windows_browser_command(&url);

        assert_eq!(command.get_program(), "rundll32.exe");
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "url.dll,FileProtocolHandler".to_owned(),
                url.as_str().to_owned()
            ]
        );
    }

    #[test]
    fn oauth_callback_matches_the_registered_cognito_url() {
        assert_eq!(OAUTH_CALLBACK_URL, "http://127.0.0.1:8765/callback");
        assert_eq!(OAUTH_CALLBACK_ADDR, "127.0.0.1:8765");
    }

    #[tokio::test]
    async fn oauth_callback_reports_when_the_registered_port_is_occupied() {
        let occupied = tokio::net::TcpListener::bind(OAUTH_CALLBACK_ADDR)
            .await
            .expect("test requires the registered callback port to start free");

        let error = bind_oauth_callback()
            .await
            .expect_err("a second listener must not claim the registered callback port");
        let message = error.to_string();
        assert!(message.contains(OAUTH_CALLBACK_URL));
        assert!(message.contains("port 8765 is available"));

        drop(occupied);
    }

    #[tokio::test]
    async fn deprecated_token_preserves_legacy_route_selection() {
        let config = ContextServiceConfig::default();
        let (_, auth) = resolve_mcp_auth(
            &config,
            Some("https://legacy.example.test/custom-route".to_owned()),
            None,
            Some("legacy-token".to_owned()),
        )
        .await
        .expect("legacy token should resolve");

        assert!(matches!(auth, ResolvedMcpAuth::LegacyBearer(_)));
    }

    #[tokio::test]
    async fn explicit_credential_file_wins_over_stale_keyring_value() {
        let temp = tempdir().expect("temp credential stores");
        let keyring = RestrictedFileCredentialStore::new(temp.path().join("keyring.json"));
        let selected = RestrictedFileCredentialStore::new(temp.path().join("selected.json"));
        let profile = CredentialProfile::new("workstation", CredentialPurpose::ApiKey)
            .expect("valid profile");
        let stale = ApiKeyCredential::parse_stdin(
            "spur_test_aaaaaaaaaaaaaaaaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        )
        .expect("stale key");
        let current = ApiKeyCredential::parse_stdin(
            "spur_test_cccccccccccccccccccccccccc_dddddddddddddddddddddddddddddddddddddddddddddddddddd",
        )
        .expect("current key");
        keyring
            .store(&profile, &StoredCredential::ApiKey(stale))
            .await
            .expect("seed stale keyring value");
        selected
            .store(&profile, &StoredCredential::ApiKey(current))
            .await
            .expect("seed selected file value");

        let resolved = resolve_api_key_with_stores(&profile, None, &keyring, Some(&selected))
            .await
            .expect("resolve selected credential")
            .expect("selected credential exists");

        assert_eq!(resolved.public_id(), "cccccccccccccccccccccccccc");
    }

    #[tokio::test]
    async fn rotated_session_is_persisted_after_remote_management_failure() {
        let server = MockServer::start().await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/oauth2/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "fresh-access-token",
                "refresh_token": "rotated-refresh-token",
                "token_type": "Bearer",
                "expires_in": 300
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(matchers::method("GET"))
            .and(matchers::path("/auth/api-keys"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;
        let discovery = DiscoveryDocument::for_test(
            server.uri(),
            "https://issuer.example.test",
            format!("{}/oauth2/authorize", server.uri()),
            format!("{}/oauth2/token", server.uri()),
            "human-client",
        )
        .expect("loopback discovery");
        let session = ManagementSession::for_test(
            "expired-access-token",
            "old-refresh-token",
            1,
            "https://issuer.example.test",
            "human-client",
        )
        .expect("expired management session");
        let client = ManagementClient::new(discovery, session).expect("management client");
        let profile = CredentialProfile::new("default", CredentialPurpose::Management)
            .expect("management profile");
        let temp = tempdir().expect("temp management store");
        let store = RestrictedFileCredentialStore::new(temp.path().join("credentials.json"));

        let operation = client.list_keys(None, Some(100)).await;
        let error = finish_management_attempt_with_store(&client, &profile, &store, operation)
            .await
            .expect_err("remote failure should remain visible");

        assert!(error.to_string().contains("management"));
        let stored = store
            .load(&profile)
            .await
            .expect("load rotated session")
            .expect("rotated session persisted");
        let StoredCredential::Management(credential) = stored else {
            panic!("management purpose should be preserved");
        };
        let session = credential.session().expect("stored session remains valid");
        assert_eq!(
            session.refresh_token().expose_secret(),
            "rotated-refresh-token"
        );
    }

    #[tokio::test]
    async fn create_storage_failure_revokes_the_remote_key() {
        let server = MockServer::start().await;
        mount_create_response(&server).await;
        Mock::given(matchers::method("DELETE"))
            .and(matchers::path("/auth/api-keys/aaaaaaaaaaaaaaaaaaaaaaaaaa"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "key_id": "aaaaaaaaaaaaaaaaaaaaaaaaaa",
                "status": "revoked"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = compensation_test_client(&server);
        let profile = CredentialProfile::new("default", CredentialPurpose::Management)
            .expect("management profile");
        let temp = tempdir().expect("temp stores");
        let management_store =
            RestrictedFileCredentialStore::new(temp.path().join("management.json"));
        let failing_api_store = RestrictedFileCredentialStore::new(temp.path());
        let request = CreateApiKeyRequest::new(
            "workstation".to_owned(),
            vec!["external.read".to_owned()],
            None,
        )
        .expect("create request");

        let error = match create_and_store_key_with_stores(
            &client,
            &profile,
            &management_store,
            &failing_api_store,
            request,
            false,
        )
        .await
        {
            Ok(_) => panic!("local storage failure should fail creation"),
            Err(error) => error,
        };

        let message = error.to_string();
        assert!(message.contains("new key was revoked"), "{message}");
        assert!(
            !message.contains(TEST_CREATED_KEY),
            "secret leaked: {message}"
        );
    }

    #[tokio::test]
    async fn create_storage_failure_reports_failed_compensation_without_secret() {
        let server = MockServer::start().await;
        mount_create_response(&server).await;
        Mock::given(matchers::method("DELETE"))
            .and(matchers::path("/auth/api-keys/aaaaaaaaaaaaaaaaaaaaaaaaaa"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;
        let client = compensation_test_client(&server);
        let profile = CredentialProfile::new("default", CredentialPurpose::Management)
            .expect("management profile");
        let temp = tempdir().expect("temp stores");
        let management_store =
            RestrictedFileCredentialStore::new(temp.path().join("management.json"));
        let failing_api_store = RestrictedFileCredentialStore::new(temp.path());
        let request = CreateApiKeyRequest::new(
            "workstation".to_owned(),
            vec!["external.read".to_owned()],
            None,
        )
        .expect("create request");

        let error = match create_and_store_key_with_stores(
            &client,
            &profile,
            &management_store,
            &failing_api_store,
            request,
            false,
        )
        .await
        {
            Ok(_) => panic!("failed compensation should remain visible"),
            Err(error) => error,
        };

        let message = error.to_string();
        assert!(message.contains("automatic revocation failed"), "{message}");
        assert!(
            !message.contains(TEST_CREATED_KEY),
            "secret leaked: {message}"
        );
    }

    const TEST_CREATED_KEY: &str =
        "spur_live_aaaaaaaaaaaaaaaaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    async fn mount_create_response(server: &MockServer) {
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/auth/api-keys"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "key": TEST_CREATED_KEY,
                "key_id": "aaaaaaaaaaaaaaaaaaaaaaaaaa",
                "name": "workstation",
                "scopes": ["external.read"],
                "created_at": 1_900_000_000_u64,
                "expires_at": 2_000_000_000_u64
            })))
            .expect(1)
            .mount(server)
            .await;
    }

    fn compensation_test_client(server: &MockServer) -> ManagementClient {
        let discovery = DiscoveryDocument::for_test(
            server.uri(),
            "https://issuer.example.test",
            format!("{}/oauth2/authorize", server.uri()),
            format!("{}/oauth2/token", server.uri()),
            "human-client",
        )
        .expect("loopback discovery");
        let session = ManagementSession::for_test(
            "fresh-access-token",
            "refresh-token",
            u64::MAX,
            "https://issuer.example.test",
            "human-client",
        )
        .expect("fresh session");
        ManagementClient::new(discovery, session).expect("management client")
    }
}
