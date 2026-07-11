use std::{sync::Arc, time::Duration};

use rand::{distributions::Alphanumeric, Rng};
use spur_context_auth_client::{M2mClient, M2mConfig, SecretString, TokenCache};
use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

fn generated_opaque_value() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

fn test_client(server: &MockServer) -> M2mClient {
    let config = M2mConfig::for_test(
        "client-a",
        SecretString::random(),
        format!("{}/oauth2/token", server.uri()),
        ["urn:spur:context-service/external.read"],
    )
    .expect("test configuration is valid");
    M2mClient::new(config).expect("client configuration is valid")
}

#[tokio::test]
async fn token_cache_single_flights_concurrent_misses_and_never_serves_expired_tokens() {
    let server = MockServer::start().await;
    let response = serde_json::json!({
        "access_token": generated_opaque_value(),
        "token_type": "Bearer",
        "expires_in": 1
    });
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/oauth2/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(50))
                .set_body_json(response),
        )
        .mount(&server)
        .await;

    let client = test_client(&server);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let client = client.clone();
        tasks.push(tokio::spawn(async move { client.access_token().await }));
    }
    for task in tasks {
        task.await
            .expect("task completes")
            .expect("same in-flight request supplies every caller");
    }

    tokio::time::sleep(Duration::from_millis(1_100)).await;
    client
        .access_token()
        .await
        .expect("expired values are replaced rather than served");
    let requests = server
        .received_requests()
        .await
        .expect("requests are recorded");
    assert_eq!(
        requests.len(),
        2,
        "one initial flight and one expired refresh"
    );
}

#[tokio::test]
async fn cache_key_isolates_client_ids_and_normalized_scope_sets() {
    let server = MockServer::start().await;
    let response = serde_json::json!({
        "access_token": generated_opaque_value(),
        "token_type": "Bearer",
        "expires_in": 300
    });
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .mount(&server)
        .await;
    let cache = Arc::new(TokenCache::default());
    let endpoint = format!("{}/oauth2/token", server.uri());
    let read_config = M2mConfig::for_test(
        "client-a",
        SecretString::random(),
        &endpoint,
        ["urn:spur:context-service/external.read"],
    )
    .expect("read configuration is valid");
    let status_config = M2mConfig::for_test(
        "client-a",
        SecretString::random(),
        &endpoint,
        ["urn:spur:context-service/external.status"],
    )
    .expect("status configuration is valid");
    let other_client_read_config = M2mConfig::for_test(
        "client-b",
        SecretString::random(),
        &endpoint,
        ["urn:spur:context-service/external.read"],
    )
    .expect("other-client read configuration is valid");
    let equivalent_read_config = M2mConfig::for_test(
        "client-a",
        SecretString::random(),
        &endpoint,
        ["urn:spur:context-service/external.read"],
    )
    .expect("equivalent read configuration is valid");

    M2mClient::new_with_cache(read_config, cache.clone())
        .expect("read client is valid")
        .access_token()
        .await
        .expect("read token is acquired");
    M2mClient::new_with_cache(status_config, cache.clone())
        .expect("status client is valid")
        .access_token()
        .await
        .expect("status token is acquired");
    M2mClient::new_with_cache(other_client_read_config, cache.clone())
        .expect("other-client read client is valid")
        .access_token()
        .await
        .expect("other-client token is acquired");
    M2mClient::new_with_cache(equivalent_read_config, cache)
        .expect("equivalent read client is valid")
        .access_token()
        .await
        .expect("equivalent read token uses cache");

    let requests = server
        .received_requests()
        .await
        .expect("requests are recorded");
    assert_eq!(
        requests.len(),
        3,
        "one request per normalized client/scope key"
    );
}
