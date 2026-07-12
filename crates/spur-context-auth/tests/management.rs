use std::time::{SystemTime, UNIX_EPOCH};

use secrecy::ExposeSecret as _;
use serde_json::json;
use spur_context_auth::{
    management::{CreateApiKeyRequest, ManagementClient, ManagementError},
    oauth::{DiscoveryDocument, ManagementSession},
};
use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn discovery(server: &MockServer) -> DiscoveryDocument {
    DiscoveryDocument::for_test(
        server.uri(),
        server.uri(),
        format!("{}/oauth2/authorize", server.uri()),
        format!("{}/oauth2/token", server.uri()),
        "human-client",
    )
    .expect("loopback discovery")
}

fn session(
    server: &MockServer,
    access_token: &str,
    refresh_token: &str,
    expires_at: u64,
) -> ManagementSession {
    ManagementSession::for_test(
        access_token,
        refresh_token,
        expires_at,
        server.uri(),
        "human-client",
    )
    .expect("bound test session")
}

#[tokio::test]
async fn expired_management_session_refreshes_before_typed_create_request() {
    let server = MockServer::start().await;
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/oauth2/token"))
        .and(matchers::body_string_contains("grant_type=refresh_token"))
        .and(matchers::body_string_contains(
            "refresh_token=refresh-secret",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "fresh-access-token",
            "refresh_token": "rotated-refresh-token",
            "token_type": "Bearer",
            "expires_in": 300
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(matchers::method("POST"))
        .and(matchers::path("/auth/api-keys"))
        .and(matchers::header("authorization", "Bearer fresh-access-token"))
        .and(matchers::body_json(json!({
            "name": "workstation",
            "scopes": ["external.read", "external.status"],
            "expires_at": 2_000_000_000_u64
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "key": "spur_live_aaaaaaaaaaaaaaaaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "key_id": "aaaaaaaaaaaaaaaaaaaaaaaaaa",
            "name": "workstation",
            "scopes": ["external.read", "external.status"],
            "created_at": 1_900_000_000_u64,
            "expires_at": 2_000_000_000_u64
        })))
        .expect(1)
        .mount(&server)
        .await;
    let session = session(
        &server,
        "expired",
        "refresh-secret",
        now().saturating_sub(1),
    );
    let client = ManagementClient::new(discovery(&server), session).expect("client");

    let created = client
        .create_key(
            CreateApiKeyRequest::new(
                "workstation",
                ["external.read", "external.status"],
                Some(2_000_000_000),
            )
            .expect("request"),
        )
        .await
        .expect("key created");

    assert_eq!(created.key_id(), "aaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert!(created.key().expose_secret().starts_with("spur_live_"));
    let debug = format!("{created:?}");
    assert!(!debug.contains("bbbbbbbb"));
    assert!(debug.contains("[REDACTED]"));
    let credential = created.into_credential();
    assert_eq!(credential.public_id(), "aaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert_eq!(
        client.session().await.refresh_token().expose_secret(),
        "rotated-refresh-token"
    );
}

#[tokio::test]
async fn list_and_revoke_use_exact_management_paths_and_typed_responses() {
    let server = MockServer::start().await;
    Mock::given(matchers::method("GET"))
        .and(matchers::path("/auth/api-keys"))
        .and(matchers::query_param("limit", "20"))
        .and(matchers::query_param("cursor", "next-page"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "keys": [{
                "key_id": "aaaaaaaaaaaaaaaaaaaaaaaaaa",
                "name": "workstation",
                "scopes": ["external.read"],
                "status": "active",
                "created_at": 1_900_000_000_u64,
                "expires_at": 2_000_000_000_u64,
                "revoked_at": null
            }],
            "next_cursor": null
        })))
        .mount(&server)
        .await;
    Mock::given(matchers::method("DELETE"))
        .and(matchers::path("/auth/api-keys/aaaaaaaaaaaaaaaaaaaaaaaaaa"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "key_id": "aaaaaaaaaaaaaaaaaaaaaaaaaa",
            "status": "revoked"
        })))
        .mount(&server)
        .await;
    let client = ManagementClient::new(
        discovery(&server),
        session(&server, "fresh", "refresh", now() + 3600),
    )
    .unwrap();

    let page = client.list_keys(Some("next-page"), Some(20)).await.unwrap();
    assert_eq!(page.keys().len(), 1);
    assert_eq!(page.keys()[0].status().as_str(), "active");
    let revoked = client
        .revoke_key("aaaaaaaaaaaaaaaaaaaaaaaaaa")
        .await
        .unwrap();
    assert_eq!(revoked.status().as_str(), "revoked");
}

#[tokio::test]
async fn remote_errors_and_oversized_bodies_are_bounded_and_redacted() {
    let server = MockServer::start().await;
    let secret_body = "server-secret-value";
    Mock::given(matchers::method("GET"))
        .and(matchers::path("/auth/api-keys"))
        .respond_with(ResponseTemplate::new(500).set_body_string(secret_body))
        .mount(&server)
        .await;
    let client = ManagementClient::new(
        discovery(&server),
        session(
            &server,
            "fresh-access-secret",
            "refresh-secret",
            now() + 3600,
        ),
    )
    .unwrap();

    let error = client
        .list_keys(None, None)
        .await
        .expect_err("bounded error");
    assert_eq!(error, ManagementError::RemoteFailure);
    assert!(!format!("{error:?}").contains(secret_body));
    assert!(!format!("{client:?}").contains("fresh-access-secret"));
    assert!(!format!("{client:?}").contains("refresh-secret"));
}

#[tokio::test]
async fn issuer_or_client_mismatch_is_rejected_before_any_request() {
    let server = MockServer::start().await;
    let wrong_issuer = ManagementSession::for_test(
        "access-secret",
        "refresh-secret",
        now().saturating_sub(1),
        "http://127.0.0.1:9",
        "human-client",
    )
    .expect("test session");
    assert_eq!(
        ManagementClient::new(discovery(&server), wrong_issuer).unwrap_err(),
        ManagementError::Authentication
    );

    let wrong_client = ManagementSession::for_test(
        "access-secret",
        "refresh-secret",
        now().saturating_sub(1),
        server.uri(),
        "other-client",
    )
    .expect("test session");
    assert_eq!(
        ManagementClient::new(discovery(&server), wrong_client).unwrap_err(),
        ManagementError::Authentication
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}
