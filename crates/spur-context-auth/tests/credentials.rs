use std::path::PathBuf;

use secrecy::ExposeSecret as _;
use spur_context_auth::credentials::CredentialStore as _;
use spur_context_auth::credentials::{
    resolve_api_key, resolve_management, ApiKeyCredential, ContextServiceAuth, CredentialError,
    CredentialProfile, CredentialPurpose, CredentialSelection, CredentialStoreSelection,
    InMemoryCredentialStore, ManagementCredential, RestrictedFileCredentialStore, StoredCredential,
};
use url::Url;

const KEYRING_KEY: &str =
    "spur_live_aaaaaaaaaaaaaaaaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const FILE_KEY: &str =
    "spur_live_cccccccccccccccccccccccccc_dddddddddddddddddddddddddddddddddddddddddddddddddddd";
const ENV_KEY: &str =
    "spur_live_eeeeeeeeeeeeeeeeeeeeeeeeee_ffffffffffffffffffffffffffffffffffffffffffffffffffff";

fn api_profile() -> CredentialProfile {
    CredentialProfile::new("workstation", CredentialPurpose::ApiKey).expect("valid profile")
}

fn management_profile() -> CredentialProfile {
    CredentialProfile::new("workstation", CredentialPurpose::Management).expect("valid profile")
}

#[tokio::test]
async fn in_memory_store_round_trips_and_deletes_purpose_separated_credentials() {
    let store = InMemoryCredentialStore::default();
    let api_key = ApiKeyCredential::parse_stdin(KEYRING_KEY).expect("canonical key");
    store
        .store(&api_profile(), &StoredCredential::ApiKey(api_key.clone()))
        .await
        .expect("store API key");

    assert_eq!(
        store.load(&api_profile()).await.expect("load API key"),
        Some(StoredCredential::ApiKey(api_key))
    );
    assert_eq!(
        store
            .load(&management_profile())
            .await
            .expect("load management"),
        None,
        "the same profile name cannot cross credential namespaces"
    );
    store.delete(&api_profile()).await.expect("delete API key");
    assert_eq!(
        store.load(&api_profile()).await.expect("load deleted"),
        None
    );
}

#[tokio::test]
async fn management_credentials_reconstruct_a_refreshable_session_after_loading() {
    let store = InMemoryCredentialStore::default();
    let management = ManagementCredential::new(
        "access-secret",
        "refresh-secret",
        2_000_000_000,
        "https://issuer.example/pool",
        "human-client",
    )
    .expect("valid management credential");
    store
        .store(
            &management_profile(),
            &StoredCredential::Management(management),
        )
        .await
        .expect("store management credential");

    let loaded = resolve_management(&management_profile(), &store, None)
        .await
        .expect("management lookup")
        .expect("management credential exists");
    let session = loaded.session().expect("stored session remains valid");
    assert_eq!(session.access_token().expose_secret(), "access-secret");
    assert_eq!(session.refresh_token().expose_secret(), "refresh-secret");
    assert_eq!(session.expires_at(), 2_000_000_000);
    assert_eq!(session.issuer().as_str(), "https://issuer.example/pool");
    assert_eq!(session.client_id(), "human-client");
    assert_eq!(loaded.issuer().as_str(), "https://issuer.example/pool");
    assert_eq!(loaded.client_id(), "human-client");
}

#[tokio::test]
async fn stores_reject_management_and_api_key_cross_contamination() {
    let store = InMemoryCredentialStore::default();
    let management = ManagementCredential::new(
        "access-secret",
        "refresh-secret",
        2_000_000_000,
        "https://issuer.example/pool",
        "human-client",
    )
    .expect("valid management credential");
    let error = store
        .store(&api_profile(), &StoredCredential::Management(management))
        .await
        .expect_err("wrong credential purpose is rejected");
    assert_eq!(error, CredentialError::PurposeMismatch);
}

#[tokio::test]
async fn api_key_resolution_precedence_is_environment_then_keyring_then_file() {
    let keyring = InMemoryCredentialStore::default();
    let file = InMemoryCredentialStore::default();
    keyring
        .store(
            &api_profile(),
            &StoredCredential::ApiKey(ApiKeyCredential::parse_stdin(KEYRING_KEY).unwrap()),
        )
        .await
        .unwrap();
    file.store(
        &api_profile(),
        &StoredCredential::ApiKey(ApiKeyCredential::parse_stdin(FILE_KEY).unwrap()),
    )
    .await
    .unwrap();

    let from_env = resolve_api_key(&api_profile(), Some(ENV_KEY), &keyring, Some(&file))
        .await
        .expect("environment key resolves")
        .expect("credential exists");
    assert_eq!(from_env.secret().expose_secret(), ENV_KEY);

    let from_keyring = resolve_api_key(&api_profile(), None, &keyring, Some(&file))
        .await
        .expect("keyring key resolves")
        .expect("credential exists");
    assert_eq!(from_keyring.secret().expose_secret(), KEYRING_KEY);

    keyring.delete(&api_profile()).await.unwrap();
    let from_file = resolve_api_key(&api_profile(), None, &keyring, Some(&file))
        .await
        .expect("file key resolves")
        .expect("credential exists");
    assert_eq!(from_file.secret().expose_secret(), FILE_KEY);
}

#[tokio::test]
async fn management_resolution_never_consults_api_key_credentials() {
    let keyring = InMemoryCredentialStore::default();
    let file = InMemoryCredentialStore::default();
    keyring
        .store(
            &api_profile(),
            &StoredCredential::ApiKey(ApiKeyCredential::parse_stdin(KEYRING_KEY).unwrap()),
        )
        .await
        .unwrap();

    assert_eq!(
        resolve_management(&management_profile(), &keyring, Some(&file))
            .await
            .expect("management lookup succeeds"),
        None
    );
}

#[test]
fn stdin_import_accepts_one_canonical_key_and_rejects_every_other_shape() {
    for accepted in [
        KEYRING_KEY.to_owned(),
        format!("{KEYRING_KEY}\n"),
        format!("{KEYRING_KEY}\r\n"),
    ] {
        let parsed = ApiKeyCredential::parse_stdin(&accepted).expect("single canonical line");
        assert_eq!(parsed.public_id(), "aaaaaaaaaaaaaaaaaaaaaaaaaa");
        assert_eq!(format!("{parsed:?}"), "ApiKeyCredential([REDACTED])");
    }

    for rejected in [
        "",
        " spur_live_aaaaaaaaaaaaaaaaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "spur_prod_aaaaaaaaaaaaaaaaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "spur_live_Aaaaaaaaaaaaaaaaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "spur_live_aaaaaaaaaaaaaaaaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb1",
        "spur_live_aaaaaaaaaaaaaaaaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\nextra",
        "spur_live_aaaaaaaaaaaaaaaaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n\n",
    ] {
        assert!(ApiKeyCredential::parse_stdin(rejected).is_err(), "{rejected:?}");
    }
}

#[tokio::test]
#[cfg(unix)]
async fn restricted_file_store_creates_0600_and_rejects_broader_permissions() {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("credentials.json");
    let store = RestrictedFileCredentialStore::new(&path);
    store
        .store(
            &api_profile(),
            &StoredCredential::ApiKey(ApiKeyCredential::parse_stdin(FILE_KEY).unwrap()),
        )
        .await
        .expect("restricted file stores credential");
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    assert!(matches!(
        store.load(&api_profile()).await.unwrap(),
        Some(StoredCredential::ApiKey(_))
    ));

    for insecure_mode in [0o644, 0o400, 0o700, 0o000] {
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(insecure_mode)).unwrap();
        assert_eq!(
            store
                .load(&api_profile())
                .await
                .expect_err("mode other than exact 0600 is rejected"),
            CredentialError::InsecureFilePermissions
        );
        assert_eq!(
            store
                .store(
                    &api_profile(),
                    &StoredCredential::ApiKey(ApiKeyCredential::parse_stdin(KEYRING_KEY).unwrap()),
                )
                .await
                .expect_err("store must not auto-restrict an insecure existing file"),
            CredentialError::InsecureFilePermissions
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().mode() & 0o777,
            insecure_mode
        );
    }
}

#[tokio::test]
async fn restricted_file_store_replaces_an_existing_credential() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("credentials.json");
    let store = RestrictedFileCredentialStore::new(&path);
    store
        .store(
            &api_profile(),
            &StoredCredential::ApiKey(ApiKeyCredential::parse_stdin(KEYRING_KEY).unwrap()),
        )
        .await
        .expect("initial credential store");
    store
        .store(
            &api_profile(),
            &StoredCredential::ApiKey(ApiKeyCredential::parse_stdin(FILE_KEY).unwrap()),
        )
        .await
        .expect("existing credential is atomically replaced");

    let Some(StoredCredential::ApiKey(loaded)) =
        store.load(&api_profile()).await.expect("load replacement")
    else {
        panic!("replacement API key exists");
    };
    assert_eq!(loaded.secret().expose_secret(), FILE_KEY);
}

#[tokio::test]
#[cfg(windows)]
async fn restricted_file_store_rejects_a_null_windows_dacl() {
    use nt_token::OwnedToken;
    use windows::Win32::Security::TOKEN_QUERY;
    use windows_permissions::{
        constants::{SeObjectType, SecurityInformation},
        wrappers::{GetNamedSecurityInfo, SetNamedSecurityInfo},
    };

    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("credentials.json");
    let store = RestrictedFileCredentialStore::new(&path);
    store
        .store(
            &api_profile(),
            &StoredCredential::ApiKey(ApiKeyCredential::parse_stdin(FILE_KEY).unwrap()),
        )
        .await
        .expect("restricted file stores credential");

    let token_owner = OwnedToken::from_current_process(TOKEN_QUERY)
        .and_then(|token| token.user())
        .and_then(|sid| sid.to_string())
        .expect("current process token SID");
    let descriptor = GetNamedSecurityInfo(
        &path.as_os_str(),
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Owner,
    )
    .expect("credential file owner");
    assert_eq!(
        descriptor.owner().expect("owner SID").to_string(),
        token_owner,
        "new credential files are explicitly owned by the process token user"
    );

    SetNamedSecurityInfo(
        &path.as_os_str(),
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl | SecurityInformation::UnprotectedDacl,
        None,
        None,
        None,
        None,
    )
    .expect("set intentionally insecure null DACL");
    assert_eq!(
        store
            .load(&api_profile())
            .await
            .expect_err("null DACL is unrestricted and must fail closed"),
        CredentialError::InsecureFilePermissions
    );
    assert_eq!(
        store
            .store(
                &api_profile(),
                &StoredCredential::ApiKey(ApiKeyCredential::parse_stdin(KEYRING_KEY).unwrap()),
            )
            .await
            .expect_err("store must not replace an insecure existing file"),
        CredentialError::InsecureFilePermissions
    );
}

#[test]
fn normal_configuration_contains_only_non_secret_selection_metadata() {
    let selection = CredentialSelection::new(
        Url::parse("https://context.example").unwrap(),
        "workstation",
        ContextServiceAuth::ApiKey,
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned()),
    )
    .expect("valid selection");
    let json = serde_json::to_value(&selection).expect("selection serializes");

    assert_eq!(json["service_url"], "https://context.example/");
    assert_eq!(json["profile"], "workstation");
    assert_eq!(json["auth_mode"], "api_key");
    assert_eq!(json["public_id_hint"], "aaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert_eq!(selection.profile(), "workstation");
    assert_eq!(selection.auth_mode(), ContextServiceAuth::ApiKey);
    assert_eq!(selection.service_url().as_str(), "https://context.example/");
    assert_eq!(
        selection.public_id_hint(),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert!(json.get("credential_file").is_none());
    assert!(serde_json::from_str::<CredentialSelection>(
        r#"{"service_url":"https://context.example/","profile":"workstation","auth_mode":"api_key","credential_file":"credentials.json"}"#,
    )
    .is_err());
}

#[test]
fn restricted_file_path_uses_explicit_store_selection_not_normal_configuration() {
    let path = PathBuf::from("/home/user/.config/spur/credentials.json");
    let selection = CredentialStoreSelection::with_restricted_file(path.clone())
        .expect("explicit restricted-file selection");

    assert_eq!(selection.restricted_file(), Some(path.as_path()));
}
