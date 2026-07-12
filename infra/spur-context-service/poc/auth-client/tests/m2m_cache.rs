use std::{
    future::{poll_fn, Future},
    sync::Arc,
    task::Poll,
    time::Duration,
};

use rand::{distributions::Alphanumeric, Rng};
use spur_context_auth_client::{
    AccessToken, ClientError, M2mClient, M2mConfig, SecretString, TokenCache,
};
use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

const REGRESSION_TIMEOUT: Duration = Duration::from_secs(2);
const CONCURRENT_CALLERS: usize = 8;

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

async fn wait_for_request_count(server: &MockServer, expected: usize) {
    tokio::time::timeout(REGRESSION_TIMEOUT, async {
        loop {
            let requests = server
                .received_requests()
                .await
                .expect("requests are recorded");
            if requests.len() >= expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("mock server observes the request before the regression timeout");
}

async fn call_concurrently(
    client: &M2mClient,
    caller_count: usize,
) -> Vec<Result<AccessToken, ClientError>> {
    let mut calls = (0..caller_count)
        .map(|_| Some(Box::pin(client.access_token())))
        .collect::<Vec<_>>();
    let mut results = (0..caller_count).map(|_| None).collect::<Vec<_>>();

    poll_fn(|context| {
        for (call, result) in calls.iter_mut().zip(&mut results) {
            let Some(future) = call else {
                continue;
            };
            if let Poll::Ready(value) = future.as_mut().poll(context) {
                *call = None;
                *result = Some(value);
            }
        }
        if calls.iter().all(Option::is_none) {
            Poll::Ready(
                results
                    .iter_mut()
                    .map(|result| result.take().expect("completed call has a result"))
                    .collect(),
            )
        } else {
            Poll::Pending
        }
    })
    .await
}

#[tokio::test(flavor = "current_thread")]
async fn concurrent_endpoint_failure_is_shared_by_every_caller() {
    let server = MockServer::start().await;
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/oauth2/token"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;

    let client = test_client(&server);
    tokio::time::timeout(
        REGRESSION_TIMEOUT,
        call_concurrently(&client, CONCURRENT_CALLERS),
    )
    .await
    .expect("all callers receive the shared endpoint failure")
    .into_iter()
    .for_each(|result| {
        assert_eq!(result.err(), Some(ClientError::TokenRequestFailed));
    });

    let requests = server
        .received_requests()
        .await
        .expect("requests are recorded");
    assert_eq!(requests.len(), 1, "one failed token flight is shared");
}

#[tokio::test]
async fn aborting_initiator_does_not_cancel_or_poison_token_acquisition() {
    let server = MockServer::start().await;
    let expected_token = generated_opaque_value();
    let response = serde_json::json!({
        "access_token": expected_token,
        "token_type": "Bearer",
        "expires_in": 300
    });
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/oauth2/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(250))
                .set_body_json(response),
        )
        .mount(&server)
        .await;

    let client = test_client(&server);
    let initiator = {
        let client = client.clone();
        tokio::spawn(async move { client.access_token().await })
    };
    wait_for_request_count(&server, 1).await;
    initiator.abort();
    assert!(
        initiator
            .await
            .expect_err("initiator is aborted")
            .is_cancelled(),
        "only the initiating caller is cancelled"
    );

    let mut waiters = Vec::new();
    for _ in 0..CONCURRENT_CALLERS {
        let client = client.clone();
        waiters.push(tokio::spawn(async move { client.access_token().await }));
    }
    tokio::time::timeout(REGRESSION_TIMEOUT, async {
        for waiter in waiters {
            let token = waiter
                .await
                .expect("waiter task completes")
                .expect("detached token acquisition succeeds");
            assert_eq!(token.as_str(), expected_token);
        }
    })
    .await
    .expect("initiator cancellation cannot hang waiters");

    let cached = tokio::time::timeout(REGRESSION_TIMEOUT, client.access_token())
        .await
        .expect("cache lookup cannot hang")
        .expect("completed detached acquisition populates the cache");
    assert_eq!(cached.as_str(), expected_token);
    let requests = server
        .received_requests()
        .await
        .expect("requests are recorded");
    assert_eq!(requests.len(), 1, "initiator cancellation cannot refetch");
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
