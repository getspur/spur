use reqwest::Client;
use serde_json::{json, Map, Value};

use crate::adapter::http::{apply_auth, cursor_value, rows_from_body};
use crate::adapter::manifest::PaginationCfg;
use crate::adapter::ResolvedAuth;
use crate::error::{GatewayError, Result};

pub struct GraphqlFetch<'a> {
    pub client: &'a Client,
    pub endpoint: &'a str,
    pub query: &'a str,
    pub variables: Map<String, Value>,
    pub auth: &'a ResolvedAuth,
    pub pagination: Option<&'a PaginationCfg>,
    pub response_path: Option<String>,
}

struct GraphqlPage {
    rows: Vec<Value>,
    body: Value,
}

async fn post_page(f: &GraphqlFetch<'_>, page_vars: Map<String, Value>) -> Result<GraphqlPage> {
    let mut variables = f.variables.clone();
    for (name, value) in page_vars {
        variables.insert(name, value);
    }

    let req = f.client.post(f.endpoint).json(&json!({
        "query": f.query,
        "variables": variables,
    }));
    let req = apply_auth(req, f.auth);

    let resp = req
        .send()
        .await
        .map_err(|e| GatewayError::Http(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(GatewayError::Http(format!("status {}", resp.status())));
    }

    let body: Value = resp
        .json()
        .await
        .map_err(|e| GatewayError::Http(e.to_string()))?;
    if let Some(errors) = body.get("errors") {
        match errors {
            Value::Null => {}
            Value::Array(values) if values.is_empty() => {}
            errors => return Err(GatewayError::Http(format!("GraphQL errors: {errors}"))),
        }
    }

    let rows = rows_from_body(&body, f.response_path.as_deref())?;
    Ok(GraphqlPage { rows, body })
}

pub async fn fetch_graphql_rows(f: &GraphqlFetch<'_>) -> Result<Vec<Value>> {
    let Some(pagination) = f.pagination else {
        return Ok(post_page(f, Map::new()).await?.rows);
    };

    let mut out = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
        let mut page_vars = Map::new();
        page_vars.insert("first".to_string(), json!(pagination.page_size));
        if let (Some(cursor_param), Some(cursor_value)) = (&pagination.cursor_param, &cursor) {
            if !cursor_param.is_empty() {
                page_vars.insert(cursor_param.clone(), Value::String(cursor_value.clone()));
            }
        }

        let page = post_page(f, page_vars).await?;
        out.extend(page.rows);

        let has_next = pagination
            .has_next_path
            .as_deref()
            .and_then(|path| cursor_value(&page.body, path))
            .as_deref()
            == Some("true");
        if !has_next {
            break;
        }

        let Some(cursor_path) = &pagination.cursor_path else {
            break;
        };
        let Some(cursor_param) = &pagination.cursor_param else {
            break;
        };
        if cursor_param.is_empty() {
            break;
        }
        let Some(next_cursor) = cursor_value(&page.body, cursor_path) else {
            break;
        };
        if next_cursor.is_empty() || cursor.as_deref() == Some(next_cursor.as_str()) {
            break;
        }
        cursor = Some(next_cursor);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use reqwest::Client;
    use serde_json::{json, Map};
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{fetch_graphql_rows, GraphqlFetch};
    use crate::adapter::manifest::PaginationCfg;
    use crate::adapter::ResolvedAuth;
    use crate::error::GatewayError;

    const QUERY: &str = "query Items($first: Int!, $after: String, $static: String!) { items(first: $first, after: $after, static: $static) { nodes { id } pageInfo { hasNextPage endCursor } } }";

    fn static_variables() -> Map<String, serde_json::Value> {
        let mut variables = Map::new();
        variables.insert("static".to_string(), json!("yes"));
        variables
    }

    #[tokio::test]
    async fn two_page_connection_paginates_and_concatenates_nodes() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_json(json!({
                "query": QUERY,
                "variables": {
                    "static": "yes",
                    "first": 2
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "items": {
                        "nodes": [{ "id": 1 }, { "id": 2 }],
                        "pageInfo": {
                            "hasNextPage": true,
                            "endCursor": "cursor-1"
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_json(json!({
                "query": QUERY,
                "variables": {
                    "static": "yes",
                    "first": 2,
                    "after": "cursor-1"
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "items": {
                        "nodes": [{ "id": 3 }],
                        "pageInfo": {
                            "hasNextPage": false,
                            "endCursor": "cursor-2"
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let client = Client::new();
        let auth = ResolvedAuth::None;
        let pagination = PaginationCfg {
            style: "cursor".to_string(),
            limit_param: None,
            offset_param: None,
            page_size: 2,
            cursor_path: Some("$.data.items.pageInfo.endCursor".to_string()),
            cursor_param: Some("after".to_string()),
            link_rel: None,
            has_next_path: Some("$.data.items.pageInfo.hasNextPage".to_string()),
        };
        let endpoint = format!("{}/graphql", server.uri());
        let fetch = GraphqlFetch {
            client: &client,
            endpoint: &endpoint,
            query: QUERY,
            variables: static_variables(),
            auth: &auth,
            pagination: Some(&pagination),
            response_path: Some("$.data.items.nodes".to_string()),
        };

        let rows = fetch_graphql_rows(&fetch).await.unwrap();

        assert_eq!(
            rows,
            vec![json!({ "id": 1 }), json!({ "id": 2 }), json!({ "id": 3 })]
        );
    }

    #[tokio::test]
    async fn graphql_errors_array_returns_http_error() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "errors": [{ "message": "boom" }],
                "data": null
            })))
            .mount(&server)
            .await;

        let client = Client::new();
        let auth = ResolvedAuth::None;
        let endpoint = format!("{}/graphql", server.uri());
        let fetch = GraphqlFetch {
            client: &client,
            endpoint: &endpoint,
            query: QUERY,
            variables: static_variables(),
            auth: &auth,
            pagination: None,
            response_path: Some("$.data.items.nodes".to_string()),
        };

        let err = fetch_graphql_rows(&fetch).await.unwrap_err();

        assert!(matches!(err, GatewayError::Http(message) if message.contains("boom")));
    }

    #[tokio::test]
    async fn without_pagination_posts_once_and_returns_rows() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/graphql"))
            .and(body_json(json!({
                "query": QUERY,
                "variables": {
                    "static": "yes"
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {
                    "items": {
                        "nodes": [{ "id": 1 }, { "id": 2 }]
                    }
                }
            })))
            .mount(&server)
            .await;

        let client = Client::new();
        let auth = ResolvedAuth::None;
        let endpoint = format!("{}/graphql", server.uri());
        let fetch = GraphqlFetch {
            client: &client,
            endpoint: &endpoint,
            query: QUERY,
            variables: static_variables(),
            auth: &auth,
            pagination: None,
            response_path: Some("$.data.items.nodes".to_string()),
        };

        let rows = fetch_graphql_rows(&fetch).await.unwrap();

        assert_eq!(rows, vec![json!({ "id": 1 }), json!({ "id": 2 })]);
        assert_eq!(server.received_requests().await.expect("requests").len(), 1);
    }
}
