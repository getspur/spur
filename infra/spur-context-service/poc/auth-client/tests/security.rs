use rand::{distributions::Alphanumeric, Rng};
use spur_context_auth_client::{ClientError, M2mClient, M2mConfig, SecretString};
use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

fn generated_opaque_value() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

fn test_client(endpoint: String) -> M2mClient {
    let config = M2mConfig::for_test(
        "client-a",
        SecretString::random(),
        endpoint,
        ["urn:spur:context-service/external.read"],
    )
    .expect("test configuration is valid");
    M2mClient::new(config).expect("client configuration is valid")
}

#[test]
fn environment_configuration_requires_all_values_without_echoing_them() {
    let result = M2mConfig::from_environment_with(|_| None);
    assert!(matches!(result, Err(ClientError::InvalidConfiguration)));
}

#[test]
fn endpoint_queries_are_rejected_before_they_can_reach_config_debug_output() {
    let result = M2mConfig::new(
        "client-a",
        SecretString::random(),
        "https://auth.example.invalid/oauth2/token?unexpected=value",
        ["urn:spur:context-service/external.read"],
    );
    assert!(matches!(result, Err(ClientError::InvalidConfiguration)));
}

#[tokio::test]
async fn token_endpoint_redirect_is_rejected_without_replaying_credentials() {
    let token_endpoint = MockServer::start().await;
    let redirect_target = MockServer::start().await;
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/oauth2/token"))
        .respond_with(
            ResponseTemplate::new(307)
                .insert_header("location", format!("{}/unexpected", redirect_target.uri())),
        )
        .mount(&token_endpoint)
        .await;

    let error = test_client(format!("{}/oauth2/token", token_endpoint.uri()))
        .access_token()
        .await
        .expect_err("redirect response is not a token response");

    assert_eq!(error, ClientError::TokenRequestFailed);
    let redirected_requests = redirect_target
        .received_requests()
        .await
        .expect("requests are recorded");
    assert!(redirected_requests.is_empty());
}

#[tokio::test]
async fn failures_and_debug_output_are_bounded_and_redacted() {
    let server = MockServer::start().await;
    let raw_response_value = generated_opaque_value();
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(400).set_body_string(raw_response_value.clone()))
        .mount(&server)
        .await;
    let secret = SecretString::random();
    let client = test_client(format!("{}/oauth2/token", server.uri()));

    let error = client
        .access_token()
        .await
        .expect_err("token endpoint failure is mapped locally");

    assert_eq!(format!("{secret:?}"), "[REDACTED]");
    assert_eq!(format!("{error:?}"), "TokenRequestFailed");
    assert!(!format!("{error:?}").contains(&raw_response_value));
}

#[tokio::test]
async fn extreme_token_lifetime_is_rejected_without_deadline_arithmetic() {
    let server = MockServer::start().await;
    let response = serde_json::json!({
        "access_token": generated_opaque_value(),
        "token_type": "Bearer",
        "expires_in": 86_401
    });
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .mount(&server)
        .await;

    let error = test_client(format!("{}/oauth2/token", server.uri()))
        .access_token()
        .await
        .expect_err("out-of-contract TTL is rejected");

    assert_eq!(error, ClientError::TokenResponseInvalid);
}
