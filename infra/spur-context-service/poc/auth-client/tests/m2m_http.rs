use rand::{distributions::Alphanumeric, Rng};
use spur_context_auth_client::{M2mClient, M2mConfig, SecretString};
use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

fn generated_secret() -> SecretString {
    SecretString::random()
}

fn generated_opaque_value() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

#[tokio::test]
async fn client_credentials_uses_basic_auth_and_exact_normalized_scopes() {
    let server = MockServer::start().await;
    let response = serde_json::json!({
        "access_token": generated_opaque_value(),
        "token_type": "Bearer",
        "expires_in": 300
    });

    Mock::given(matchers::method("POST"))
        .and(matchers::path("/oauth2/token"))
        .and(matchers::header_exists("authorization"))
        .and(matchers::body_string_contains("grant_type=client_credentials"))
        .and(matchers::body_string_contains("scope=urn%3Aspur%3Acontext-service%2Fexternal.read+urn%3Aspur%3Acontext-service%2Fexternal.status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .mount(&server)
        .await;

    let config = M2mConfig::for_test(
        "client-a",
        generated_secret(),
        format!("{}/oauth2/token", server.uri()),
        [
            "urn:spur:context-service/external.status",
            "urn:spur:context-service/external.read",
            "urn:spur:context-service/external.read",
        ],
    )
    .expect("test configuration is valid");
    let client = M2mClient::new(config).expect("client configuration is valid");

    let access_token = client.access_token().await.expect("token request succeeds");

    assert!(!access_token.as_str().is_empty());
    let requests = server
        .received_requests()
        .await
        .expect("requests are recorded");
    let request = requests.first().expect("one token request is made");
    let authorization = request
        .headers
        .get("authorization")
        .expect("Basic authorization header is present")
        .to_str()
        .expect("header is valid ASCII");
    assert!(authorization.starts_with("Basic "));
    assert!(!String::from_utf8_lossy(&request.body).contains("client_secret"));
    assert_eq!(requests.len(), 1, "one client-credentials request is made");
}
