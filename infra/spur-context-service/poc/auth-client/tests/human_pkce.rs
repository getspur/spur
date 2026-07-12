use spur_context_auth_client::{ClientError, HumanClient, HumanConfig};
use url::Url;

fn query_value(url: &Url, name: &str) -> String {
    url.query_pairs()
        .find_map(|(key, value)| (key == name).then_some(value.into_owned()))
        .expect("authorization URL includes required parameter")
}

fn test_client() -> HumanClient {
    let config = HumanConfig::for_test(
        "https://issuer.example.invalid",
        "http://127.0.0.1:41001/oauth2/authorize",
        "http://127.0.0.1:41001/oauth2/token",
        "human-client",
        "http://127.0.0.1:41002/callback",
    )
    .expect("test configuration is valid");
    HumanClient::new(config).expect("human client configuration is valid")
}

#[test]
fn authorization_attempts_use_fresh_s256_pkce_state_and_nonce() {
    let client = test_client();
    let mut first = client
        .begin_authorization(["openid", "urn:spur:context-service/external.read"])
        .expect("authorization URL is created");
    let second = client
        .begin_authorization(["openid", "urn:spur:context-service/external.read"])
        .expect("authorization URL is created");

    assert_eq!(
        query_value(first.authorization_url(), "code_challenge_method"),
        "S256"
    );
    assert!(!query_value(first.authorization_url(), "code_challenge").is_empty());
    assert!(
        query_value(first.authorization_url(), "code_challenge")
            != query_value(second.authorization_url(), "code_challenge")
    );
    assert!(!query_value(first.authorization_url(), "state").is_empty());
    assert!(!query_value(first.authorization_url(), "nonce").is_empty());
    assert!(
        query_value(first.authorization_url(), "state")
            != query_value(second.authorization_url(), "state")
    );
    assert!(
        query_value(first.authorization_url(), "nonce")
            != query_value(second.authorization_url(), "nonce")
    );

    assert_eq!(
        first.validate_callback_state("wrong-state"),
        Err(ClientError::StateRejected)
    );
    let returned_state = query_value(first.authorization_url(), "state");
    assert_eq!(first.validate_callback_state(&returned_state), Ok(()));
    assert_eq!(
        first.validate_callback_state(&returned_state),
        Err(ClientError::AuthorizationAlreadyUsed)
    );
}
