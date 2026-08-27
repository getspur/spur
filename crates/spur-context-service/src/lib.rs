//! DuckLake-backed code context service for external packages.

#[cfg(feature = "service")]
pub mod abuse;
pub mod api_key_authorizer;
pub mod api_key_cleanup;
pub mod api_keys;
pub mod artifact_cache;
#[cfg(any(feature = "service", feature = "lambda-http"))]
mod auth;
#[cfg(feature = "service")]
pub mod catalog;
#[cfg(feature = "code-backend")]
pub mod code_backend;
#[cfg(feature = "service")]
pub mod drainer;
#[cfg(feature = "service")]
pub mod jobs;
#[cfg(feature = "service")]
pub mod knowledge;
#[cfg(feature = "lambda")]
pub mod lambda;
#[cfg(feature = "lambda-http")]
mod lambda_http;
#[cfg(feature = "service")]
pub mod mcp;
pub mod medallion;
#[cfg(feature = "service")]
pub mod query;
pub mod serving_registry;
#[cfg(feature = "service")]
pub mod staleness;
#[cfg(feature = "service")]
pub mod translate;
#[cfg(feature = "worker")]
pub mod worker;

#[cfg(all(test, feature = "lambda-http"))]
mod lambda_http_contract {
    use serde_json::json;

    use crate::auth::{AuthFailure, RequestRoute};
    use crate::lambda_http::{
        authenticated_caller_id, classify_route, reject_api_key_auth_on_wrong_route,
        reject_jwt_auth_on_wrong_route, ApiGatewayRequest,
    };

    fn request(path: &str, method: &str) -> ApiGatewayRequest {
        serde_json::from_value(json!({
            "rawPath": path,
            "requestContext": { "http": { "method": method } }
        }))
        .expect("API Gateway request should deserialize")
    }

    fn jwt_request(path: &str, method: &str) -> ApiGatewayRequest {
        serde_json::from_value(json!({
            "rawPath": path,
            "requestContext": {
                "http": { "method": method },
                "authorizer": { "jwt": { "claims": { "sub": "caller" } } }
            }
        }))
        .expect("JWT-authorized API Gateway request should deserialize")
    }

    fn api_key_request(path: &str, method: &str) -> ApiGatewayRequest {
        serde_json::from_value(json!({
            "rawPath": path,
            "requestContext": {
                "http": { "method": method },
                "authorizer": { "lambda": { "principalId": "key-owner" } }
            }
        }))
        .expect("API-key-authorized API Gateway request should deserialize")
    }

    #[test]
    fn oauth_path_classifies_through_lambda_http() {
        assert_eq!(
            classify_route(&request("/mcp/oauth", "POST")),
            RequestRoute::OAuth
        );
    }

    #[test]
    fn api_key_path_classifies_through_lambda_http() {
        assert_eq!(
            classify_route(&request("/mcp/api-key", "POST")),
            RequestRoute::ApiKeyMcp
        );
    }

    #[test]
    fn management_paths_classify_through_lambda_http() {
        assert_eq!(
            classify_route(&request("/auth/api-keys", "POST")),
            RequestRoute::ApiKeyCreate
        );
        assert_eq!(
            classify_route(&request("/auth/api-keys", "GET")),
            RequestRoute::ApiKeyList
        );
        assert_eq!(
            classify_route(&request("/auth/api-keys/key-123", "DELETE")),
            RequestRoute::ApiKeyRevoke
        );
    }

    #[test]
    fn direct_tool_paths_keep_legacy_route_classification() {
        for path in ["/mcp/index", "/mcp/index_status"] {
            assert_eq!(classify_route(&request(path, "POST")), RequestRoute::Legacy);
        }
    }

    #[test]
    fn direct_unauthenticated_paths_keep_legacy_route_classification() {
        for path in ["/mcp", "/mcp/code", "/mcp/knowledge"] {
            assert_eq!(classify_route(&request(path, "POST")), RequestRoute::Legacy);
        }
    }

    #[test]
    fn jwt_route_guard_accepts_exact_oauth_splits_and_rejects_other_routes() {
        for path in ["/mcp/oauth/code", "/mcp/oauth/knowledge"] {
            assert_eq!(
                reject_jwt_auth_on_wrong_route(&jwt_request(path, "POST")),
                Ok(())
            );
        }

        for request in [
            jwt_request("/mcp/api-key/code", "POST"),
            jwt_request("/mcp/code", "POST"),
            jwt_request("/mcp/oauth/code", "GET"),
            jwt_request("/mcp/oauth/code/extra", "POST"),
        ] {
            assert_eq!(
                reject_jwt_auth_on_wrong_route(&request),
                Err(AuthFailure::WrongRoute)
            );
        }
    }

    #[test]
    fn api_key_route_guard_accepts_exact_api_key_splits_and_rejects_other_routes() {
        for path in ["/mcp/api-key/code", "/mcp/api-key/knowledge"] {
            assert_eq!(
                reject_api_key_auth_on_wrong_route(&api_key_request(path, "POST")),
                Ok(())
            );
        }

        for request in [
            api_key_request("/mcp/oauth/code", "POST"),
            api_key_request("/mcp/code", "POST"),
            api_key_request("/mcp/api-key/code", "GET"),
            api_key_request("/mcp/api-key/code/extra", "POST"),
        ] {
            assert_eq!(
                reject_api_key_auth_on_wrong_route(&request),
                Err(AuthFailure::WrongRoute)
            );
        }
    }

    #[test]
    fn unauthenticated_mutating_caller_error_keeps_legacy_message() {
        assert_eq!(
            authenticated_caller_id(&request("/mcp", "POST"), false)
                .expect_err("unauthenticated mutation should be rejected")
                .to_string(),
            "authenticated caller is required for mutating context-service tools"
        );
    }
}
