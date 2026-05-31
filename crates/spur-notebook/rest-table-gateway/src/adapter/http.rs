use reqwest::Client;
use serde_json::Value;

use crate::adapter::manifest::PaginationCfg;
use crate::adapter::ResolvedAuth;
use crate::error::{GatewayError, Result};

pub struct HttpFetch<'a> {
    pub client: &'a Client,
    pub base_url: &'a str,
    pub path: &'a str,
    pub query: Vec<(String, String)>,
    pub pagination: Option<&'a PaginationCfg>,
    pub auth: &'a ResolvedAuth,
}

async fn get_page(f: &HttpFetch<'_>, extra: &[(String, String)]) -> Result<Vec<Value>> {
    let url = format!("{}{}", f.base_url.trim_end_matches('/'), f.path);
    let mut req = f.client.get(url).query(&f.query).query(extra);

    match f.auth {
        ResolvedAuth::None => {}
        ResolvedAuth::Bearer(t) => {
            req = req.bearer_auth(t);
        }
        ResolvedAuth::Header { name, value } => {
            req = req.header(name.as_str(), value.as_str());
        }
    }

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
    match body {
        Value::Array(a) => Ok(a),
        other => Err(GatewayError::Http(format!(
            "expected JSON array, got {}",
            other
        ))),
    }
}

pub async fn fetch_rows(f: &HttpFetch<'_>) -> Result<Vec<Value>> {
    let Some(p) = f.pagination else {
        return get_page(f, &[]).await;
    };

    // v1 treats every configured pagination style as offset pagination.
    let mut out = Vec::new();
    let mut offset: u32 = 0;
    loop {
        let extra = vec![
            (p.limit_param.clone(), p.page_size.to_string()),
            (p.offset_param.clone(), offset.to_string()),
        ];
        let page = get_page(f, &extra).await?;
        let n = page.len() as u32;
        out.extend(page);

        if n < p.page_size {
            break;
        }
        offset += p.page_size;
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use reqwest::Client;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{fetch_rows, HttpFetch};
    use crate::adapter::manifest::PaginationCfg;
    use crate::adapter::ResolvedAuth;

    #[tokio::test]
    async fn paginates_until_short_page() {
        let server = MockServer::start().await;

        let first_page: Vec<_> = (0..500).map(|id| json!({ "id": id })).collect();
        Mock::given(method("GET"))
            .and(path("/markets"))
            .and(query_param("offset", "0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(first_page))
            .mount(&server)
            .await;

        let second_page: Vec<_> = (500..503).map(|id| json!({ "id": id })).collect();
        Mock::given(method("GET"))
            .and(path("/markets"))
            .and(query_param("offset", "500"))
            .respond_with(ResponseTemplate::new(200).set_body_json(second_page))
            .mount(&server)
            .await;

        let client = Client::new();
        let pagination = PaginationCfg {
            style: "offset".to_string(),
            limit_param: "limit".to_string(),
            offset_param: "offset".to_string(),
            page_size: 500,
        };
        let auth = ResolvedAuth::None;
        let fetch = HttpFetch {
            client: &client,
            base_url: &server.uri(),
            path: "/markets",
            query: Vec::new(),
            pagination: Some(&pagination),
            auth: &auth,
        };

        let rows = fetch_rows(&fetch).await.unwrap();

        assert_eq!(rows.len(), 503);
    }

    #[tokio::test]
    async fn bearer_auth_header_sent() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/markets"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&server)
            .await;

        let client = Client::new();
        let auth = ResolvedAuth::Bearer("tok".to_string());
        let fetch = HttpFetch {
            client: &client,
            base_url: &server.uri(),
            path: "/markets",
            query: Vec::new(),
            pagination: None,
            auth: &auth,
        };

        let rows = fetch_rows(&fetch).await.unwrap();

        assert!(rows.is_empty());
    }
}
