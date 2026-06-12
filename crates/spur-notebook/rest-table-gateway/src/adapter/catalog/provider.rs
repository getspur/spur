use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::adapter::nango::{parse_providers, Paginate, ProviderEntry, Verification};

const NANGO_LICENSE: &str = "Elastic License 2.0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCatalogEntry {
    pub provider: String,
    pub display_name: String,
    pub categories: Vec<String>,
    pub auth_mode: Option<String>,
    pub base_url: Option<String>,
    pub connection_config_keys: Vec<String>,
    pub credential_keys: Vec<String>,
    pub proxy_headers: IndexMap<String, String>,
    pub proxy_query: IndexMap<String, String>,
    pub proxy_body: IndexMap<String, String>,
    pub pagination: Option<NangoPagination>,
    pub verification: Vec<VerificationEndpoint>,
    pub authorization_url: Option<String>,
    pub token_url: Option<String>,
    pub docs_endpoints: Vec<String>,
    pub seed_class: ProviderSeedClass,
    pub nango_license: String,
    pub nango_commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NangoPagination {
    pub kind: Option<String>,
    pub cursor_path_in_response: Option<String>,
    pub cursor_name_in_request: Option<String>,
    pub response_path: Option<String>,
    pub limit_name_in_request: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationEndpoint {
    pub method: Option<String>,
    pub endpoint: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProviderSeedClass {
    BaseUrlOnly,
    RestCollectionLikeDocsEndpoint,
    RestSingletonOrUnknownDocsEndpoint,
    VerificationEndpointOnly,
    GraphqlCandidate,
    MetadataOnly,
}

pub fn provider_catalog_from_yaml(
    yaml: &str,
    nango_commit: &str,
) -> Result<Vec<ProviderCatalogEntry>, serde_yaml::Error> {
    let providers = parse_providers(yaml)?;
    let mut entries = providers
        .into_iter()
        .map(|(provider, entry)| normalize_provider(provider, entry, nango_commit))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.provider.cmp(&right.provider));
    Ok(entries)
}

fn normalize_provider(
    provider: String,
    entry: ProviderEntry,
    nango_commit: &str,
) -> ProviderCatalogEntry {
    let proxy = entry.proxy.as_ref();
    let base_url = proxy.and_then(|proxy| proxy.base_url.clone());
    let proxy_headers = proxy
        .and_then(|proxy| proxy.headers.clone())
        .unwrap_or_default();
    let proxy_query = proxy
        .and_then(|proxy| proxy.query.clone())
        .unwrap_or_default();
    let proxy_body = proxy
        .and_then(|proxy| proxy.body.clone())
        .unwrap_or_default();
    let pagination = proxy
        .and_then(|proxy| proxy.paginate.as_ref())
        .map(NangoPagination::from);
    let verification = proxy
        .and_then(|proxy| proxy.verification.as_ref())
        .map(verification_endpoints)
        .unwrap_or_default();
    let docs_endpoints = docs_endpoints(entry.docs.as_ref());
    let seed_class = classify_seed(base_url.as_deref(), &docs_endpoints, &verification);

    ProviderCatalogEntry {
        provider: provider.clone(),
        display_name: entry.display_name.unwrap_or_else(|| provider.clone()),
        categories: entry.categories.unwrap_or_default(),
        auth_mode: entry.auth_mode,
        base_url,
        connection_config_keys: sorted_keys(entry.connection_config.as_ref()),
        credential_keys: sorted_keys(entry.credentials.as_ref()),
        proxy_headers,
        proxy_query,
        proxy_body,
        pagination,
        verification,
        authorization_url: entry.authorization_url,
        token_url: entry.token_url,
        docs_endpoints,
        seed_class,
        nango_license: NANGO_LICENSE.to_string(),
        nango_commit: nango_commit.to_string(),
    }
}

fn classify_seed(
    base_url: Option<&str>,
    docs_endpoints: &[String],
    verification: &[VerificationEndpoint],
) -> ProviderSeedClass {
    if base_url
        .into_iter()
        .chain(docs_endpoints.iter().map(String::as_str))
        .any(|value| value.to_ascii_lowercase().contains("graphql"))
    {
        return ProviderSeedClass::GraphqlCandidate;
    }

    if !docs_endpoints.is_empty() {
        if docs_endpoints
            .iter()
            .any(|endpoint| collection_like(endpoint))
        {
            return ProviderSeedClass::RestCollectionLikeDocsEndpoint;
        }
        return ProviderSeedClass::RestSingletonOrUnknownDocsEndpoint;
    }

    if base_url.is_some() {
        return ProviderSeedClass::BaseUrlOnly;
    }

    if !verification.is_empty() {
        return ProviderSeedClass::VerificationEndpointOnly;
    }

    ProviderSeedClass::MetadataOnly
}

fn collection_like(endpoint: &str) -> bool {
    let trimmed = endpoint.trim_end_matches('/');
    let Some(segment) = trimmed.rsplit('/').next() else {
        return false;
    };
    let segment = segment
        .split(['?', '#'])
        .next()
        .unwrap_or(segment)
        .trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .to_ascii_lowercase();

    !matches!(
        segment.as_str(),
        "" | "me" | "my" | "self" | "profile" | "account" | "user" | "current"
    ) && (segment.ends_with('s') || segment.contains("{id}") || segment.contains(":id"))
}

fn docs_endpoints(docs: Option<&serde_yaml::Value>) -> Vec<String> {
    let mut endpoints = Vec::new();
    if let Some(docs) = docs {
        collect_docs_strings(docs, &mut endpoints);
    }
    endpoints.sort();
    endpoints.dedup();
    endpoints
}

fn collect_docs_strings(value: &serde_yaml::Value, endpoints: &mut Vec<String>) {
    match value {
        serde_yaml::Value::String(value) => {
            if looks_like_endpoint(value) {
                endpoints.push(value.clone());
            }
        }
        serde_yaml::Value::Sequence(items) => {
            for item in items {
                collect_docs_strings(item, endpoints);
            }
        }
        serde_yaml::Value::Mapping(map) => {
            for value in map.values() {
                collect_docs_strings(value, endpoints);
            }
        }
        _ => {}
    }
}

fn looks_like_endpoint(value: &str) -> bool {
    let value = value.trim();
    value.starts_with('/') && !value.starts_with("//")
}

fn verification_endpoints(verification: &Verification) -> Vec<VerificationEndpoint> {
    match verification {
        Verification::One(endpoint) => verification_endpoint_rows(endpoint),
        Verification::Many(endpoints) => endpoints
            .iter()
            .flat_map(verification_endpoint_rows)
            .collect(),
    }
}

fn verification_endpoint_rows(
    endpoint: &crate::adapter::nango::VerificationEndpoint,
) -> Vec<VerificationEndpoint> {
    let mut rows = Vec::new();
    if let Some(path) = &endpoint.endpoint {
        rows.push(VerificationEndpoint {
            method: endpoint.method.clone(),
            endpoint: Some(path.clone()),
        });
    }
    if let Some(endpoints) = &endpoint.endpoints {
        rows.extend(endpoints.iter().map(|path| VerificationEndpoint {
            method: endpoint.method.clone(),
            endpoint: Some(path.clone()),
        }));
    }
    if rows.is_empty() {
        rows.push(VerificationEndpoint {
            method: endpoint.method.clone(),
            endpoint: None,
        });
    }
    rows
}

fn sorted_keys(map: Option<&IndexMap<String, serde_yaml::Value>>) -> Vec<String> {
    let mut keys = map
        .into_iter()
        .flat_map(|map| map.keys().cloned())
        .collect::<Vec<_>>();
    keys.sort();
    keys
}

impl From<&Paginate> for NangoPagination {
    fn from(value: &Paginate) -> Self {
        Self {
            kind: value.kind.clone(),
            cursor_path_in_response: value.cursor_path_in_response.clone(),
            cursor_name_in_request: value.cursor_name_in_request.clone(),
            response_path: value.response_path.clone(),
            limit_name_in_request: value.limit_name_in_request.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry<'a>(entries: &'a [ProviderCatalogEntry], provider: &str) -> &'a ProviderCatalogEntry {
        entries
            .iter()
            .find(|entry| entry.provider == provider)
            .unwrap_or_else(|| panic!("{provider} provider is present"))
    }

    #[test]
    fn normalize_provider_keeps_auth_proxy_and_license_metadata() {
        let yaml = r#"
github:
  display_name: GitHub
  categories: [dev-tools]
  auth_mode: OAUTH2
  authorization_url: https://github.com/login/oauth/authorize
  token_url: https://github.com/login/oauth/access_token
  proxy:
    base_url: https://api.github.com
    headers:
      X-GitHub-Api-Version: "2022-11-28"
    query:
      api-version: "2022-11-28"
    body:
      audience: "${connectionConfig.audience}"
    verification:
      method: GET
      endpoint: /user
"#;
        let entries = provider_catalog_from_yaml(yaml, "988efd014").expect("catalog parses");
        let github = entry(&entries, "github");

        assert_eq!(github.display_name, "GitHub");
        assert_eq!(github.categories, ["dev-tools".to_string()]);
        assert_eq!(github.base_url.as_deref(), Some("https://api.github.com"));
        assert_eq!(github.auth_mode.as_deref(), Some("OAUTH2"));
        assert_eq!(
            github.authorization_url.as_deref(),
            Some("https://github.com/login/oauth/authorize")
        );
        assert_eq!(
            github.token_url.as_deref(),
            Some("https://github.com/login/oauth/access_token")
        );
        assert_eq!(
            github
                .proxy_headers
                .get("X-GitHub-Api-Version")
                .map(String::as_str),
            Some("2022-11-28")
        );
        assert_eq!(
            github.proxy_query.get("api-version").map(String::as_str),
            Some("2022-11-28")
        );
        assert_eq!(
            github.proxy_body.get("audience").map(String::as_str),
            Some("${connectionConfig.audience}")
        );
        assert_eq!(github.verification.len(), 1);
        assert_eq!(github.nango_license, "Elastic License 2.0");
        assert_eq!(github.nango_commit, "988efd014");
    }

    #[test]
    fn normalize_provider_expands_plural_verification_endpoints() {
        let yaml = r#"
events:
  display_name: Events
  proxy:
    verification:
      method: GET
      endpoints:
        - /api/v2/auth/introspect
        - /api/v2/users/me
"#;
        let entries = provider_catalog_from_yaml(yaml, "988efd014").expect("catalog parses");
        let events = entry(&entries, "events");

        assert_eq!(
            events.verification,
            [
                VerificationEndpoint {
                    method: Some("GET".to_string()),
                    endpoint: Some("/api/v2/auth/introspect".to_string()),
                },
                VerificationEndpoint {
                    method: Some("GET".to_string()),
                    endpoint: Some("/api/v2/users/me".to_string()),
                },
            ]
        );
    }

    #[test]
    fn api_key_provider_extracts_config_credentials_and_pagination() {
        let yaml = r#"
sendgrid-api-key:
  display_name: SendGrid API Key
  categories: [marketing]
  auth_mode: API_KEY
  credentials:
    apiKey:
      type: string
  connection_config:
    region:
      type: string
  proxy:
    base_url: https://api.${connectionConfig.region}.sendgrid.com/v3
    headers:
      Authorization: "Bearer ${apiKey}"
    paginate:
      type: cursor
      cursor_path_in_response: next
      cursor_name_in_request: cursor
"#;
        let entries = provider_catalog_from_yaml(yaml, "988efd014").expect("catalog parses");
        let sendgrid = entry(&entries, "sendgrid-api-key");

        assert_eq!(sendgrid.seed_class, ProviderSeedClass::BaseUrlOnly);
        assert_eq!(sendgrid.connection_config_keys, ["region".to_string()]);
        assert_eq!(sendgrid.credential_keys, ["apiKey".to_string()]);
        assert_eq!(
            sendgrid
                .pagination
                .as_ref()
                .and_then(|pagination| pagination.kind.as_deref()),
            Some("cursor")
        );
    }

    #[test]
    fn classifies_base_url_only_and_verification_only_providers() {
        let yaml = r#"
base-only:
  display_name: Base Only
  proxy:
    base_url: https://api.example.com
verification-only:
  display_name: Verification Only
  proxy:
    verification:
      method: GET
      endpoint: /me
"#;
        let entries = provider_catalog_from_yaml(yaml, "988efd014").expect("catalog parses");

        assert_eq!(
            entry(&entries, "base-only").seed_class,
            ProviderSeedClass::BaseUrlOnly
        );
        assert_eq!(
            entry(&entries, "verification-only").seed_class,
            ProviderSeedClass::VerificationEndpointOnly
        );
    }

    #[test]
    fn classifies_docs_endpoints_by_collection_shape() {
        let yaml = r#"
collection-docs:
  display_name: Collection Docs
  docs:
    - /api/users
singleton-docs:
  display_name: Singleton Docs
  docs:
    - /api/me
"#;
        let entries = provider_catalog_from_yaml(yaml, "988efd014").expect("catalog parses");

        assert_eq!(
            entry(&entries, "collection-docs").seed_class,
            ProviderSeedClass::RestCollectionLikeDocsEndpoint
        );
        assert_eq!(
            entry(&entries, "singleton-docs").seed_class,
            ProviderSeedClass::RestSingletonOrUnknownDocsEndpoint
        );
    }

    #[test]
    fn official_docs_url_does_not_create_docs_endpoint_seed() {
        let yaml = r#"
onepassword-events:
  display_name: 1Password Events
  docs: https://nango.dev/docs/api-integrations/1password-events
  proxy:
    base_url: https://events.1password.com
"#;
        let entries = provider_catalog_from_yaml(yaml, "988efd014").expect("catalog parses");
        let provider = entry(&entries, "onepassword-events");

        assert!(provider.docs_endpoints.is_empty());
        assert_eq!(provider.seed_class, ProviderSeedClass::BaseUrlOnly);
    }

    #[test]
    fn classifies_graphql_candidate_before_rest_seeds() {
        let yaml = r#"
graphql-provider:
  display_name: GraphQL Provider
  proxy:
    base_url: https://api.example.com/graphql
"#;
        let entries = provider_catalog_from_yaml(yaml, "988efd014").expect("catalog parses");

        assert_eq!(
            entry(&entries, "graphql-provider").seed_class,
            ProviderSeedClass::GraphqlCandidate
        );
    }

    #[test]
    fn classifies_metadata_only_provider() {
        let yaml = r#"
metadata-only:
  display_name: Metadata Only
  categories: [productivity]
"#;
        let entries = provider_catalog_from_yaml(yaml, "988efd014").expect("catalog parses");

        assert_eq!(
            entry(&entries, "metadata-only").seed_class,
            ProviderSeedClass::MetadataOnly
        );
    }
}
