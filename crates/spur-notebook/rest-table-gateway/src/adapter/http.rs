use reqwest::header::{HeaderMap, LINK};
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
    pub response_path: Option<String>,
}

struct HttpPage {
    rows: Vec<Value>,
    body: Value,
    next_link: Option<String>,
}

pub struct HttpAction<'a> {
    pub client: &'a Client,
    pub method: reqwest::Method,
    pub url: String,
    pub query: Vec<(String, String)>,
    pub body: Option<serde_json::Value>,
    pub auth: &'a ResolvedAuth,
    /// (header name, value) - attached verbatim when present.
    pub idempotency_key: Option<(String, String)>,
}

/// Issues exactly one request. No retry, no pagination.
/// Returns (status, parsed body). 204 / empty body -> Value::Null.
pub async fn send_request(a: &HttpAction<'_>) -> Result<(u16, serde_json::Value)> {
    let mut req = a.client.request(a.method.clone(), &a.url).query(&a.query);
    if let Some(body) = &a.body {
        req = req.json(body);
    }
    if let Some((name, value)) = &a.idempotency_key {
        req = req.header(name.as_str(), value.as_str());
    }
    let req = apply_auth(req, a.auth);

    let resp = req
        .send()
        .await
        .map_err(|e| GatewayError::Http(e.to_string()))?;
    let status = resp.status();

    if !status.is_success() {
        let snippet = resp.text().await.unwrap_or_default();
        let snippet: String = snippet.chars().take(500).collect();
        return Err(GatewayError::Http(format!("status {status}: {snippet}")));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(|e| GatewayError::Http(e.to_string()))?;
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).map_err(|e| GatewayError::Http(e.to_string()))?
    };

    Ok((status.as_u16(), body))
}

pub(crate) fn json_path_get<'a>(row: &'a Value, path: &str) -> Option<&'a Value> {
    let p = path.strip_prefix("$.").unwrap_or(path);
    let mut cur = row;
    for seg in p.split('.') {
        cur = cur.get(seg)?;
    }
    if cur.is_null() {
        None
    } else {
        Some(cur)
    }
}

pub(crate) fn rows_from_body(body: &Value, response_path: Option<&str>) -> Result<Vec<Value>> {
    let value = match response_path {
        Some(path) => json_path_get(body, path).ok_or_else(|| {
            GatewayError::Http(format!("expected JSON array at {path}, got null"))
        })?,
        None => body,
    };

    value.as_array().cloned().ok_or_else(|| {
        let target = response_path
            .map(|path| format!(" at {path}"))
            .unwrap_or_default();
        GatewayError::Http(format!("expected JSON array{target}, got {value}"))
    })
}

pub(crate) fn cursor_value(body: &Value, path: &str) -> Option<String> {
    match json_path_get(body, path)? {
        Value::String(value) => Some(value.clone()),
        Value::Null => None,
        value => Some(value.to_string()),
    }
}

fn next_link(headers: &HeaderMap, rel: &str) -> Option<String> {
    headers
        .get_all(LINK)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find_map(|value| next_link_from_header(value, rel))
}

fn next_link_from_header(header: &str, rel: &str) -> Option<String> {
    for part in header.split(',') {
        let mut segments = part.split(';');
        let url = segments.next()?.trim();
        let url = url.strip_prefix('<')?.strip_suffix('>')?;

        for segment in segments {
            let segment = segment.trim();
            let Some(value) = segment.strip_prefix("rel=") else {
                continue;
            };
            let value = value.trim_matches('"');
            if value.split_whitespace().any(|candidate| candidate == rel) {
                return Some(url.to_string());
            }
        }
    }

    None
}

pub(crate) fn apply_auth(
    req: reqwest::RequestBuilder,
    auth: &ResolvedAuth,
) -> reqwest::RequestBuilder {
    match auth {
        ResolvedAuth::None => req,
        ResolvedAuth::Bearer(t) => req.bearer_auth(t),
        ResolvedAuth::Header { name, value } => req.header(name.as_str(), value.as_str()),
        ResolvedAuth::Basic { user, pass } => req.basic_auth(user, Some(pass)),
        ResolvedAuth::QueryParam { param, value } => req.query(&[(param.as_str(), value.as_str())]),
    }
}

async fn get_page(
    f: &HttpFetch<'_>,
    extra: &[(String, String)],
    url_override: Option<&str>,
    link_rel: Option<&str>,
) -> Result<HttpPage> {
    let url = url_override
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}{}", f.base_url.trim_end_matches('/'), f.path));
    let mut req = f.client.get(url);
    if url_override.is_none() {
        req = req.query(&f.query).query(extra);
    }

    let req = apply_auth(req, f.auth);

    let resp = req
        .send()
        .await
        .map_err(|e| GatewayError::Http(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(GatewayError::Http(format!("status {}", resp.status())));
    }
    let next_link = link_rel.and_then(|rel| next_link(resp.headers(), rel));

    let body: Value = resp
        .json()
        .await
        .map_err(|e| GatewayError::Http(e.to_string()))?;
    let rows = rows_from_body(&body, f.response_path.as_deref())?;

    Ok(HttpPage {
        rows,
        body,
        next_link,
    })
}

pub async fn fetch_rows(f: &HttpFetch<'_>) -> Result<Vec<Value>> {
    let Some(p) = f.pagination else {
        return Ok(get_page(f, &[], None, None).await?.rows);
    };

    match p.style.as_str() {
        "cursor" => fetch_cursor_rows(f, p).await,
        "link" => fetch_link_rows(f, p).await,
        _ => fetch_offset_rows(f, p).await,
    }
}

async fn fetch_offset_rows(f: &HttpFetch<'_>, p: &PaginationCfg) -> Result<Vec<Value>> {
    let (Some(limit_param), Some(offset_param)) = (&p.limit_param, &p.offset_param) else {
        return Ok(get_page(f, &[], None, None).await?.rows);
    };
    if p.page_size == 0 {
        return Ok(get_page(f, &[], None, None).await?.rows);
    }

    let mut out = Vec::new();
    let mut offset: u32 = 0;
    loop {
        let extra = vec![
            (limit_param.clone(), p.page_size.to_string()),
            (offset_param.clone(), offset.to_string()),
        ];
        let page = get_page(f, &extra, None, None).await?;
        let n = page.rows.len() as u32;
        out.extend(page.rows);

        if n < p.page_size {
            break;
        }
        offset += p.page_size;
    }

    Ok(out)
}

async fn fetch_cursor_rows(f: &HttpFetch<'_>, p: &PaginationCfg) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;

    loop {
        let extra = match (&p.cursor_param, &cursor) {
            (Some(param), Some(value)) => vec![(param.clone(), value.clone())],
            _ => Vec::new(),
        };
        let page = get_page(f, &extra, None, None).await?;
        out.extend(page.rows);

        let Some(cursor_path) = &p.cursor_path else {
            break;
        };
        let Some(cursor_param) = &p.cursor_param else {
            break;
        };
        let Some(next_cursor) = cursor_value(&page.body, cursor_path) else {
            break;
        };
        if next_cursor.is_empty() || cursor.as_deref() == Some(next_cursor.as_str()) {
            break;
        }

        cursor = Some(next_cursor);
        if cursor_param.is_empty() {
            break;
        }
    }

    Ok(out)
}

async fn fetch_link_rows(f: &HttpFetch<'_>, p: &PaginationCfg) -> Result<Vec<Value>> {
    let rel = p.link_rel.as_deref().unwrap_or("next");
    let mut out = Vec::new();
    let mut next_url: Option<String> = None;

    loop {
        let page = get_page(f, &[], next_url.as_deref(), Some(rel)).await?;
        out.extend(page.rows);
        next_url = page.next_link;

        if next_url.is_none() {
            break;
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use reqwest::Client;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{fetch_rows, send_request, HttpAction, HttpFetch};
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
            limit_param: Some("limit".to_string()),
            offset_param: Some("offset".to_string()),
            page_size: 500,
            cursor_path: None,
            cursor_param: None,
            link_rel: None,
            has_next_path: None,
        };
        let auth = ResolvedAuth::None;
        let fetch = HttpFetch {
            client: &client,
            base_url: &server.uri(),
            path: "/markets",
            query: Vec::new(),
            pagination: Some(&pagination),
            auth: &auth,
            response_path: None,
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
            response_path: None,
        };

        let rows = fetch_rows(&fetch).await.unwrap();

        assert!(rows.is_empty());
    }

    #[tokio::test]
    async fn envelope_extracts_response_path() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/items"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{ "id": 1 }, { "id": 2 }]
            })))
            .mount(&server)
            .await;

        let client = Client::new();
        let auth = ResolvedAuth::None;
        let fetch = HttpFetch {
            client: &client,
            base_url: &server.uri(),
            path: "/items",
            query: Vec::new(),
            pagination: None,
            auth: &auth,
            response_path: Some("$.data".to_string()),
        };

        let rows = fetch_rows(&fetch).await.unwrap();

        assert_eq!(rows.len(), 2);
    }

    #[tokio::test]
    async fn cursor_pagination() {
        let server = MockServer::start().await;

        let first_page: Vec<_> = (0..500).map(|id| json!({ "id": id })).collect();
        Mock::given(method("GET"))
            .and(path("/items"))
            .and(query_param_is_missing("cursor"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": first_page,
                "next": "abc"
            })))
            .mount(&server)
            .await;

        let second_page: Vec<_> = (500..503).map(|id| json!({ "id": id })).collect();
        Mock::given(method("GET"))
            .and(path("/items"))
            .and(query_param("cursor", "abc"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": second_page
            })))
            .mount(&server)
            .await;

        let client = Client::new();
        let pagination = PaginationCfg {
            style: "cursor".to_string(),
            limit_param: None,
            offset_param: None,
            page_size: 500,
            cursor_path: Some("$.next".to_string()),
            cursor_param: Some("cursor".to_string()),
            link_rel: None,
            has_next_path: None,
        };
        let auth = ResolvedAuth::None;
        let fetch = HttpFetch {
            client: &client,
            base_url: &server.uri(),
            path: "/items",
            query: Vec::new(),
            pagination: Some(&pagination),
            auth: &auth,
            response_path: Some("$.items".to_string()),
        };

        let rows = fetch_rows(&fetch).await.unwrap();

        assert_eq!(rows.len(), 503);
    }

    #[tokio::test]
    async fn link_pagination() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/items"))
            .and(query_param_is_missing("page"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header(
                        "link",
                        format!("<{}/items?page=2>; rel=\"next\"", server.uri()),
                    )
                    .set_body_json(json!([{ "id": 1 }, { "id": 2 }])),
            )
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/items"))
            .and(query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([{ "id": 3 }])))
            .mount(&server)
            .await;

        let client = Client::new();
        let pagination = PaginationCfg {
            style: "link".to_string(),
            limit_param: None,
            offset_param: None,
            page_size: 0,
            cursor_path: None,
            cursor_param: None,
            link_rel: None,
            has_next_path: None,
        };
        let auth = ResolvedAuth::None;
        let fetch = HttpFetch {
            client: &client,
            base_url: &server.uri(),
            path: "/items",
            query: Vec::new(),
            pagination: Some(&pagination),
            auth: &auth,
            response_path: None,
        };

        let rows = fetch_rows(&fetch).await.unwrap();

        assert_eq!(rows.len(), 3);
    }

    #[tokio::test]
    async fn post_sends_body_and_returns_parsed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/orders/abc"))
            .and(header("idempotency-key", "key-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "o1" })))
            .mount(&server)
            .await;

        let client = Client::new();
        let auth = ResolvedAuth::None;
        let action = HttpAction {
            client: &client,
            method: reqwest::Method::POST,
            url: format!("{}/orders/abc", server.uri()),
            query: vec![("verbose".to_string(), "true".to_string())],
            body: Some(json!({ "price": 0.5 })),
            auth: &auth,
            idempotency_key: Some(("Idempotency-Key".to_string(), "key-1".to_string())),
        };

        let (status, body) = send_request(&action).await.unwrap();
        assert_eq!(status, 200);
        assert_eq!(body["id"], "o1");
    }

    #[tokio::test]
    async fn delete_204_returns_null_body() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/orders/abc"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let client = Client::new();
        let auth = ResolvedAuth::None;
        let action = HttpAction {
            client: &client,
            method: reqwest::Method::DELETE,
            url: format!("{}/orders/abc", server.uri()),
            query: vec![],
            body: None,
            auth: &auth,
            idempotency_key: None,
        };

        let (status, body) = send_request(&action).await.unwrap();
        assert_eq!(status, 204);
        assert!(body.is_null());
    }

    #[tokio::test]
    async fn non_2xx_is_error_with_status() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/orders/abc"))
            .respond_with(ResponseTemplate::new(422).set_body_json(json!({ "error": "bad" })))
            .mount(&server)
            .await;

        let client = Client::new();
        let auth = ResolvedAuth::None;
        let action = HttpAction {
            client: &client,
            method: reqwest::Method::PATCH,
            url: format!("{}/orders/abc", server.uri()),
            query: vec![],
            body: Some(json!({ "price": 1.0 })),
            auth: &auth,
            idempotency_key: None,
        };

        let err = send_request(&action).await.unwrap_err();
        assert!(format!("{err}").contains("422"));
    }
}
