//! DuckLake-backed code context service for external packages.

#[cfg(feature = "service")]
pub mod abuse;
pub mod api_key_authorizer;
pub mod api_key_cleanup;
pub mod api_keys;
#[cfg(feature = "service")]
mod auth;
#[cfg(feature = "service")]
pub mod catalog;
#[cfg(feature = "service")]
pub mod drainer;
#[cfg(feature = "service")]
pub mod jobs;
#[cfg(feature = "service")]
pub mod knowledge;
#[cfg(feature = "lambda")]
pub mod lambda;
#[cfg(feature = "lambda")]
mod lambda_http;
#[cfg(feature = "service")]
pub mod mcp;
#[cfg(feature = "service")]
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

#[cfg(all(test, feature = "lambda"))]
mod lambda_http_contract {
    use serde_json::json;

    use crate::auth::RequestRoute;
    use crate::lambda_http::{classify_route, ApiGatewayRequest};

    fn request(path: &str, method: &str) -> ApiGatewayRequest {
        serde_json::from_value(json!({
            "rawPath": path,
            "requestContext": { "http": { "method": method } }
        }))
        .expect("API Gateway request should deserialize")
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
}
