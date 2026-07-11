use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use openidconnect::{
    core::{
        CoreJsonWebKeySet, CoreJwsSigningAlgorithm, CoreProviderMetadata, CoreResponseType,
        CoreSubjectIdentifierType,
    },
    AuthUrl, EmptyAdditionalProviderMetadata, IssuerUrl, JsonWebKeySetUrl, ResponseTypes, TokenUrl,
};
use rand::{distributions::Alphanumeric, Rng};
use rsa::{
    pkcs8::{EncodePrivateKey, LineEnding},
    traits::PublicKeyParts,
    RsaPrivateKey,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use spur_context_auth_client::{ClientError, HumanClient, HumanConfig};
use url::Url;
use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

fn generated_opaque_value() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

fn query_value(url: &Url, name: &str) -> String {
    url.query_pairs()
        .find_map(|(key, value)| (key == name).then_some(value.into_owned()))
        .expect("authorization URL contains the required parameter")
}

fn access_token_hash(access_token: &str) -> String {
    let digest = Sha256::digest(access_token.as_bytes());
    URL_SAFE_NO_PAD.encode(&digest[..digest.len() / 2])
}

fn signed_id_token(
    issuer: &str,
    audience: &str,
    nonce: &str,
    access_token: &str,
    signing_key: &EncodingKey,
    corrupt: Option<&str>,
) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock is after the Unix epoch")
        .as_secs();
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("mock-signing-key".to_owned());
    let payload = json!({
        "iss": if corrupt == Some("issuer") { "https://wrong-issuer.invalid" } else { issuer },
        "sub": generated_opaque_value(),
        "aud": if corrupt == Some("audience") { "wrong-client" } else { audience },
        "exp": now + 300,
        "iat": now,
        "nonce": if corrupt == Some("nonce") { "wrong-nonce" } else { nonce },
        "at_hash": if corrupt == Some("hash") { generated_opaque_value() } else { access_token_hash(access_token) }
    });
    encode(&header, &payload, signing_key).expect("runtime-generated key signs the ID token")
}

async fn assert_id_token_validation(corrupt: Option<&str>) {
    let server = MockServer::start().await;
    let private_key =
        RsaPrivateKey::new(&mut rand::thread_rng(), 2048).expect("runtime RSA key is generated");
    let key_pem = private_key
        .to_pkcs8_pem(LineEnding::LF)
        .expect("private key encodes to PEM");
    let signing_key = EncodingKey::from_rsa_pem(key_pem.as_bytes()).expect("PEM key loads");
    let alternate_signing_key = (corrupt == Some("signature")).then(|| {
        let private_key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048)
            .expect("runtime alternate RSA key is generated");
        let key_pem = private_key
            .to_pkcs8_pem(LineEnding::LF)
            .expect("alternate private key encodes to PEM");
        EncodingKey::from_rsa_pem(key_pem.as_bytes()).expect("alternate PEM key loads")
    });
    let jwks = json!({
        "keys": [{
            "kty": "RSA",
            "kid": "mock-signing-key",
            "use": "sig",
            "alg": "RS256",
            "n": URL_SAFE_NO_PAD.encode(private_key.n().to_bytes_be()),
            "e": URL_SAFE_NO_PAD.encode(private_key.e().to_bytes_be())
        }]
    });
    let jwks: CoreJsonWebKeySet = serde_json::from_value(jwks).expect("generated JWKS parses");
    let metadata = CoreProviderMetadata::new(
        IssuerUrl::new(server.uri()).expect("loopback issuer parses"),
        AuthUrl::new(format!("{}/oauth2/authorize", server.uri()))
            .expect("authorization URL parses"),
        JsonWebKeySetUrl::new("https://metadata.invalid/jwks".to_owned()).expect("JWKS URL parses"),
        vec![ResponseTypes::new(vec![CoreResponseType::Code])],
        vec![CoreSubjectIdentifierType::Public],
        vec![CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256],
        EmptyAdditionalProviderMetadata {},
    )
    .set_token_endpoint(Some(
        TokenUrl::new(format!("{}/oauth2/token", server.uri())).expect("loopback token URL parses"),
    ))
    .set_jwks(jwks);

    let config = HumanConfig::for_test(
        server.uri(),
        format!("{}/oauth2/authorize", server.uri()),
        format!("{}/oauth2/token", server.uri()),
        "human-client",
        "http://127.0.0.1:41002/callback",
    )
    .expect("test configuration is valid");
    let client = HumanClient::with_provider_metadata_for_test(config, metadata)
        .expect("client configuration is valid");
    let mut pending = client
        .begin_authorization(["openid"])
        .expect("authorization attempt is created");
    let state = query_value(pending.authorization_url(), "state");
    let nonce = query_value(pending.authorization_url(), "nonce");
    let access_token = generated_opaque_value();
    let id_token = signed_id_token(
        &server.uri(),
        "human-client",
        &nonce,
        &access_token,
        alternate_signing_key.as_ref().unwrap_or(&signing_key),
        corrupt,
    );
    let response = json!({
        "access_token": access_token,
        "token_type": "Bearer",
        "expires_in": Duration::from_secs(300).as_secs(),
        "id_token": id_token
    });
    let token_response = if corrupt == Some("verifier") {
        ResponseTemplate::new(400).set_body_string(generated_opaque_value())
    } else {
        ResponseTemplate::new(200).set_body_json(response)
    };
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/oauth2/token"))
        .and(matchers::body_string_contains("code_verifier="))
        .respond_with(token_response)
        .mount(&server)
        .await;

    let result = client
        .finish_authorization(&mut pending, generated_opaque_value(), &state)
        .await;
    let requests = server
        .received_requests()
        .await
        .expect("requests are recorded");
    assert_eq!(requests.len(), 1, "one PKCE-bound code exchange");
    if corrupt == Some("verifier") {
        assert_eq!(result.err(), Some(ClientError::TokenRequestFailed));
    } else if corrupt.is_some() {
        assert_eq!(result.err(), Some(ClientError::OidcVerificationFailed));
    } else {
        assert!(
            result
                .expect("valid ID token is accepted")
                .access_token()
                .as_str()
                .len()
                > 1
        );
    }
}

#[tokio::test]
async fn oidc_validation_accepts_a_signed_token_with_matching_nonce_and_hash() {
    assert_id_token_validation(None).await;
}

#[tokio::test]
async fn oidc_validation_rejects_wrong_issuer_audience_nonce_hash_signature_and_verifier() {
    for corrupt in [
        "issuer",
        "audience",
        "nonce",
        "hash",
        "signature",
        "verifier",
    ] {
        assert_id_token_validation(Some(corrupt)).await;
    }
}
