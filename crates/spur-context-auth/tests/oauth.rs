use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use openidconnect::{
    core::{
        CoreJsonWebKeySet, CoreJwsSigningAlgorithm, CoreProviderMetadata, CoreResponseType,
        CoreSubjectIdentifierType,
    },
    AuthUrl, EmptyAdditionalProviderMetadata, IssuerUrl, JsonWebKeySetUrl, ResponseTypes, TokenUrl,
};
use rand::{distributions::Alphanumeric, Rng as _};
use rsa::{
    pkcs8::{EncodePrivateKey as _, LineEnding},
    traits::PublicKeyParts as _,
    RsaPrivateKey,
};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use spur_context_auth::oauth::{
    secure_http_client_for_test, DiscoveryDocument, HumanClient, HumanConfig, OAuthError,
};
use url::Url;
use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

fn query_value(url: &Url, name: &str) -> String {
    url.query_pairs()
        .find_map(|(key, value)| (key == name).then_some(value.into_owned()))
        .expect("authorization URL includes required parameter")
}

fn opaque() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

#[test]
fn discovery_validates_schema_https_endpoints_and_exact_service_origin() {
    let service = Url::parse("https://context.example").unwrap();
    let valid = json!({
        "schema_version": 1,
        "issuer": "https://issuer.example/pool",
        "human_client_id": "human-client",
        "authorization_endpoint": "https://auth.example/oauth2/authorize",
        "token_endpoint": "https://auth.example/oauth2/token",
        "supported_scopes": ["urn:spur:context-service/keys.manage"],
        "api_key_auth_enabled": true,
        "api_key_mcp_url": "https://context.example/mcp/api-key",
        "api_key_management_url": "https://context.example/auth/api-keys"
    });
    let discovery = DiscoveryDocument::from_json_for_service(&valid.to_string(), &service)
        .expect("valid discovery");
    assert_eq!(discovery.human_client_id(), "human-client");
    assert!(discovery.api_key_auth_enabled());

    let mut disabled = valid.clone();
    disabled["api_key_auth_enabled"] = json!(false);
    let disabled = DiscoveryDocument::from_json_for_service(&disabled.to_string(), &service)
        .expect("disabled feature status remains valid discovery");
    assert!(
        !disabled.api_key_auth_enabled(),
        "callers must be able to fail closed before API-key operations"
    );

    let invalid = [
        ("schema_version", json!(2)),
        ("issuer", json!("http://issuer.example/pool")),
        (
            "token_endpoint",
            json!("https://user@auth.example/oauth2/token"),
        ),
        (
            "api_key_management_url",
            json!("https://attacker.example/auth/api-keys"),
        ),
        (
            "api_key_mcp_url",
            json!("https://context.example/mcp/api-key/"),
        ),
    ];
    for (field, value) in invalid {
        let mut invalid = valid.clone();
        invalid[field] = value;
        assert_eq!(
            DiscoveryDocument::from_json_for_service(&invalid.to_string(), &service).unwrap_err(),
            OAuthError::InvalidDiscovery
        );
    }
}

fn test_config(server: &MockServer) -> HumanConfig {
    HumanConfig::for_test(
        server.uri(),
        format!("{}/oauth2/authorize", server.uri()),
        format!("{}/oauth2/token", server.uri()),
        "human-client",
        "http://127.0.0.1:41002/callback",
    )
    .expect("test configuration")
}

#[tokio::test]
async fn authorization_uses_fresh_s256_state_nonce_and_exact_one_shot_callback() {
    let server = MockServer::start().await;
    let client = HumanClient::new(test_config(&server)).expect("human client");
    let mut first = client
        .begin_authorization(["openid", "urn:spur:context-service/keys.manage"])
        .expect("first attempt");
    let second = client
        .begin_authorization(["openid", "urn:spur:context-service/keys.manage"])
        .expect("second attempt");

    assert_eq!(
        query_value(first.authorization_url(), "code_challenge_method"),
        "S256"
    );
    for name in ["code_challenge", "state", "nonce"] {
        assert_ne!(
            query_value(first.authorization_url(), name),
            query_value(second.authorization_url(), name),
            "{name} is fresh"
        );
    }
    let state = query_value(first.authorization_url(), "state");
    assert_eq!(
        first
            .parse_callback(
                &Url::parse(&format!(
                    "http://127.0.0.1:41003/callback?code=x&state={state}"
                ))
                .unwrap()
            )
            .unwrap_err(),
        OAuthError::CallbackRejected
    );
    assert_eq!(
        first
            .parse_callback(
                &Url::parse(&format!(
                    "http://127.0.0.1:41002/callback/?code=x&state={state}"
                ))
                .unwrap()
            )
            .unwrap_err(),
        OAuthError::CallbackRejected
    );
    let exact = Url::parse(&format!(
        "http://127.0.0.1:41002/callback?code=opaque-code&state={state}"
    ))
    .unwrap();
    assert!(first.parse_callback(&exact).is_ok());
    assert_eq!(
        first.parse_callback(&exact).unwrap_err(),
        OAuthError::AuthorizationAlreadyUsed
    );
}

fn access_token_hash(access_token: &str) -> String {
    let digest = Sha256::digest(access_token.as_bytes());
    URL_SAFE_NO_PAD.encode(&digest[..digest.len() / 2])
}

fn signed_id_token(
    issuer: &str,
    nonce: &str,
    access_token: &str,
    signing_key: &EncodingKey,
    corrupt: Option<&str>,
) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("mock-signing-key".to_owned());
    let mut payload = json!({
        "iss": if corrupt == Some("issuer") { "https://wrong.invalid" } else { issuer },
        "sub": opaque(),
        "aud": if corrupt == Some("audience") { "wrong-client" } else { "human-client" },
        "exp": now + 300,
        "iat": now,
        "nonce": if corrupt == Some("nonce") { "wrong-nonce" } else { nonce },
        "at_hash": if corrupt == Some("hash") { opaque() } else { access_token_hash(access_token) }
    });
    if corrupt == Some("missing_hash") {
        payload.as_object_mut().unwrap().remove("at_hash");
    }
    encode(&header, &payload, signing_key).expect("sign token")
}

async fn oidc_case(corrupt: Option<&str>) -> Result<(), OAuthError> {
    let server = MockServer::start().await;
    let private_key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
    let pem = private_key.to_pkcs8_pem(LineEnding::LF).unwrap();
    let signing_key = EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap();
    let alternate = (corrupt == Some("signature")).then(|| {
        let key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let pem = key.to_pkcs8_pem(LineEnding::LF).unwrap();
        EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap()
    });
    let jwks: CoreJsonWebKeySet = serde_json::from_value(json!({"keys": [{
        "kty": "RSA", "kid": "mock-signing-key", "use": "sig", "alg": "RS256",
        "n": URL_SAFE_NO_PAD.encode(private_key.n().to_bytes_be()),
        "e": URL_SAFE_NO_PAD.encode(private_key.e().to_bytes_be())
    }]}))
    .unwrap();
    let metadata = CoreProviderMetadata::new(
        IssuerUrl::new(server.uri()).unwrap(),
        AuthUrl::new(format!("{}/oauth2/authorize", server.uri())).unwrap(),
        JsonWebKeySetUrl::new("https://metadata.invalid/jwks".to_owned()).unwrap(),
        vec![ResponseTypes::new(vec![CoreResponseType::Code])],
        vec![CoreSubjectIdentifierType::Public],
        vec![CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256],
        EmptyAdditionalProviderMetadata {},
    )
    .set_token_endpoint(Some(
        TokenUrl::new(format!("{}/oauth2/token", server.uri())).unwrap(),
    ))
    .set_jwks(jwks);
    let client = HumanClient::with_provider_metadata_for_test(test_config(&server), metadata)?;
    let mut pending = client.begin_authorization(["openid"])?;
    let nonce = query_value(pending.authorization_url(), "nonce");
    let state = query_value(pending.authorization_url(), "state");
    let access_token = opaque();
    let id_token = signed_id_token(
        &server.uri(),
        &nonce,
        &access_token,
        alternate.as_ref().unwrap_or(&signing_key),
        corrupt,
    );
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/oauth2/token"))
        .and(matchers::body_string_contains("code_verifier="))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": access_token,
            "refresh_token": opaque(),
            "token_type": "Bearer",
            "expires_in": 300,
            "id_token": id_token
        })))
        .mount(&server)
        .await;
    let callback = pending.parse_callback(
        &Url::parse(&format!(
            "http://127.0.0.1:41002/callback?code={}&state={state}",
            opaque()
        ))
        .unwrap(),
    )?;
    client.finish_authorization(&mut pending, callback).await?;
    Ok(())
}

#[tokio::test]
async fn oidc_strictly_checks_issuer_audience_signature_nonce_and_access_token_hash() {
    oidc_case(None).await.expect("valid signed token");
    for corrupt in [
        "issuer",
        "audience",
        "signature",
        "nonce",
        "hash",
        "missing_hash",
    ] {
        assert_eq!(
            oidc_case(Some(corrupt)).await.unwrap_err(),
            OAuthError::OidcVerificationFailed
        );
    }
}

#[tokio::test]
async fn oauth_http_client_rejects_redirects_and_enforces_request_timeout() {
    let source = MockServer::start().await;
    let target = MockServer::start().await;
    Mock::given(matchers::path("/redirect"))
        .respond_with(ResponseTemplate::new(307).insert_header("location", target.uri()))
        .mount(&source)
        .await;
    Mock::given(matchers::path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(200)))
        .mount(&source)
        .await;
    let client = secure_http_client_for_test(Duration::from_millis(50)).expect("client");

    assert_eq!(
        client
            .get(format!("{}/redirect", source.uri()))
            .send()
            .await
            .unwrap()
            .status(),
        307
    );
    assert!(target.received_requests().await.unwrap().is_empty());
    assert!(client
        .get(format!("{}/slow", source.uri()))
        .send()
        .await
        .is_err());
}
