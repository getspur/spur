use indexmap::IndexMap;
use serde::Deserialize;

use crate::adapter::manifest::{AuthCfg, Manifest, PaginationCfg, SourceCfg, Transport};

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderEntry {
    pub display_name: Option<String>,
    pub categories: Option<Vec<String>>,
    pub auth_mode: Option<String>,
    pub token_url: Option<String>,
    // Authorization-code (Approach C) provider fields. Parsed now; consumed by the
    // deferred "Connect with browser" wizard task. Optional so non-OAuth providers
    // (and the SAMPLE / OAUTH2_CC / TWO_STEP entries) keep parsing unchanged.
    pub authorization_url: Option<String>,
    #[serde(default)]
    pub authorization_params: Option<IndexMap<String, String>>,
    pub scope_separator: Option<String>,
    pub proxy: Option<Proxy>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Proxy {
    pub base_url: Option<String>,
    pub headers: Option<IndexMap<String, String>>,
    pub paginate: Option<Paginate>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Paginate {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub cursor_path_in_response: Option<String>,
    pub cursor_name_in_request: Option<String>,
    pub response_path: Option<String>,
    pub limit_name_in_request: Option<String>,
}

pub fn parse_providers(
    yaml: &str,
) -> std::result::Result<IndexMap<String, ProviderEntry>, serde_yaml::Error> {
    serde_yaml::from_str(yaml)
}

pub fn provider_to_manifest_stub(name: &str, p: &ProviderEntry) -> Manifest {
    let upper = env_prefix(name);
    let proxy = p.proxy.as_ref();
    let base_url = proxy
        .and_then(|proxy| proxy.base_url.clone())
        .unwrap_or_default();

    Manifest {
        source: SourceCfg {
            name: name.to_string(),
            base_url: base_url.clone(),
            transport: Transport::Rest,
            auth: auth_cfg(name, p, &upper),
            pagination: proxy
                .and_then(|proxy| proxy.paginate.as_ref())
                .and_then(pagination_cfg),
            connection_config: connection_config_names(&base_url),
            allow_writes: false,
            headers: IndexMap::new(),
        },
        tables: Vec::new(),
        actions: Vec::new(),
    }
}

pub fn manifest_to_toml(m: &Manifest) -> String {
    let mut out = String::new();
    out.push_str(
        "# Generated from Nango providers.yaml (Elastic License 2.0). Modified/derived file. See THIRD_PARTY_NOTICES.\n",
    );
    out.push_str("# TODO: add [[table]] blocks (path/columns/filters)\n\n");
    out.push_str("[source]\n");
    out.push_str(&format!("name = {}\n", toml_string(&m.source.name)));
    out.push_str(&format!("base_url = {}\n", toml_string(&m.source.base_url)));

    if !m.source.connection_config.is_empty() {
        out.push_str(&format!(
            "connection_config = [{}]\n",
            m.source
                .connection_config
                .iter()
                .map(|name| toml_string(name))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    out.push_str(&format!("auth = {}\n", auth_to_toml(&m.source.auth)));

    if let Some(pagination) = &m.source.pagination {
        out.push_str(&format!(
            "pagination = {}\n",
            pagination_to_toml(pagination)
        ));
    }

    out
}

pub fn tier(auth_mode: Option<&str>) -> char {
    match auth_mode.map(normalize_auth_mode).as_deref() {
        Some("API_KEY" | "BASIC" | "NONE") => 'A',
        Some("OAUTH2" | "OAUTH2_CC" | "TWO_STEP" | "MCP_OAUTH2" | "MCP_OAUTH2_GENERIC") => 'B',
        _ => 'C',
    }
}

fn auth_cfg(name: &str, p: &ProviderEntry, upper: &str) -> AuthCfg {
    match p.auth_mode.as_deref().map(normalize_auth_mode).as_deref() {
        Some("API_KEY") => {
            let env = format!("{upper}_API_KEY");
            if let Some((header, _)) = api_key_header(p) {
                AuthCfg::Header {
                    name: header.to_string(),
                    env,
                }
            } else {
                AuthCfg::Bearer { env }
            }
        }
        Some("BASIC") => AuthCfg::Basic {
            user_env: format!("{upper}_USER"),
            pass_env: format!("{upper}_PASS"),
        },
        _ => {
            if let Some(token_url) = p.token_url.clone() {
                AuthCfg::Oauth2Refresh {
                    token_url,
                    client_id_env: format!("{upper}_CLIENT_ID"),
                    client_secret_env: format!("{upper}_CLIENT_SECRET"),
                    refresh_token_env: format!("{upper}_REFRESH_TOKEN"),
                    scope: None,
                }
            } else {
                AuthCfg::Bearer {
                    env: format!("{}_TOKEN", env_prefix(name)),
                }
            }
        }
    }
}

fn api_key_header(p: &ProviderEntry) -> Option<(&str, &str)> {
    p.proxy
        .as_ref()?
        .headers
        .as_ref()?
        .iter()
        .find(|(_, value)| value.contains("${apiKey}"))
        .map(|(name, value)| (name.as_str(), value.as_str()))
}

fn pagination_cfg(p: &Paginate) -> Option<PaginationCfg> {
    match p.kind.as_deref()?.trim().to_ascii_lowercase().as_str() {
        "cursor" => Some(PaginationCfg {
            style: "cursor".to_string(),
            limit_param: None,
            offset_param: None,
            page_size: 0,
            cursor_path: p.cursor_path_in_response.clone(),
            cursor_param: p.cursor_name_in_request.clone(),
            link_rel: None,
            has_next_path: None,
        }),
        "offset" => Some(PaginationCfg {
            style: "offset".to_string(),
            limit_param: p.limit_name_in_request.clone(),
            offset_param: p
                .cursor_name_in_request
                .clone()
                .or_else(|| Some("offset".to_string())),
            page_size: 0,
            cursor_path: None,
            cursor_param: None,
            link_rel: None,
            has_next_path: None,
        }),
        "link" => Some(PaginationCfg {
            style: "link".to_string(),
            limit_param: None,
            offset_param: None,
            page_size: 0,
            cursor_path: None,
            cursor_param: None,
            link_rel: p.response_path.clone(),
            has_next_path: None,
        }),
        _ => None,
    }
}

fn connection_config_names(base_url: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = base_url;
    let marker = "${connectionConfig.";

    while let Some(start) = rest.find(marker) {
        let after_marker = &rest[start + marker.len()..];
        let Some(end) = after_marker.find('}') else {
            break;
        };

        let name = &after_marker[..end];
        if !name.is_empty() && !names.iter().any(|seen| seen == name) {
            names.push(name.to_string());
        }
        rest = &after_marker[end + 1..];
    }

    names
}

fn env_prefix(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn normalize_auth_mode(mode: &str) -> String {
    mode.trim().to_ascii_uppercase().replace('-', "_")
}

fn auth_to_toml(auth: &AuthCfg) -> String {
    match auth {
        AuthCfg::None => "{ scheme = \"none\" }".to_string(),
        AuthCfg::Bearer { env } => {
            format!("{{ scheme = \"bearer\", env = {} }}", toml_string(env))
        }
        AuthCfg::Header { name, env } => format!(
            "{{ scheme = \"header\", name = {}, env = {} }}",
            toml_string(name),
            toml_string(env)
        ),
        AuthCfg::Basic { user_env, pass_env } => format!(
            "{{ scheme = \"basic\", user_env = {}, pass_env = {} }}",
            toml_string(user_env),
            toml_string(pass_env)
        ),
        AuthCfg::ApiKeyQuery { param, env } => format!(
            "{{ scheme = \"api_key_query\", param = {}, env = {} }}",
            toml_string(param),
            toml_string(env)
        ),
        AuthCfg::Oauth2Refresh {
            token_url,
            client_id_env,
            client_secret_env,
            refresh_token_env,
            scope,
        } => {
            let mut out = format!(
                "{{ scheme = \"oauth2_refresh\", token_url = {}, client_id_env = {}, client_secret_env = {}, refresh_token_env = {}",
                toml_string(token_url),
                toml_string(client_id_env),
                toml_string(client_secret_env),
                toml_string(refresh_token_env)
            );
            if let Some(scope) = scope {
                out.push_str(&format!(", scope = {}", toml_string(scope)));
            }
            out.push_str(" }");
            out
        }
    }
}

fn pagination_to_toml(p: &PaginationCfg) -> String {
    let mut fields = vec![
        format!("style = {}", toml_string(&p.style)),
        format!("page_size = {}", p.page_size),
    ];

    if let Some(limit_param) = &p.limit_param {
        fields.push(format!("limit_param = {}", toml_string(limit_param)));
    }
    if let Some(offset_param) = &p.offset_param {
        fields.push(format!("offset_param = {}", toml_string(offset_param)));
    }
    if let Some(cursor_path) = &p.cursor_path {
        fields.push(format!("cursor_path = {}", toml_string(cursor_path)));
    }
    if let Some(cursor_param) = &p.cursor_param {
        fields.push(format!("cursor_param = {}", toml_string(cursor_param)));
    }
    if let Some(link_rel) = &p.link_rel {
        fields.push(format!("link_rel = {}", toml_string(link_rel)));
    }

    format!("{{ {} }}", fields.join(", "))
}

fn toml_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len() + 2);
    escaped.push('"');
    for c in value.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\u{08}' => escaped.push_str("\\b"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\u{0c}' => escaped.push_str("\\f"),
            '\r' => escaped.push_str("\\r"),
            c if c.is_control() => escaped.push_str(&format!("\\u{:04X}", c as u32)),
            c => escaped.push(c),
        }
    }
    escaped.push('"');
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
stripe:
  display_name: Stripe
  categories:
    - payments
    - analytics
  auth_mode: API_KEY
  proxy:
    base_url: "https://api.stripe.com/${connectionConfig.account}/v1"
    headers:
      x-api-key: "${apiKey}"
github:
  display_name: GitHub
  categories:
    - dev-tools
  auth_mode: BASIC
  proxy:
    base_url: "https://api.github.com"
salesforce:
  display_name: Salesforce
  categories:
    - sales
  auth_mode: OAUTH2
  proxy:
    base_url: "https://example.my.salesforce.com"
    paginate:
      type: cursor
      cursor_path_in_response: "$.next_cursor"
      cursor_name_in_request: cursor
"#;

    #[test]
    fn parse_counts() {
        let providers = parse_providers(SAMPLE).expect("providers yaml should parse");

        assert_eq!(providers.len(), 3);
    }

    #[test]
    fn api_key_maps_to_header() {
        let providers = parse_providers(SAMPLE).expect("providers yaml should parse");
        let manifest = provider_to_manifest_stub("stripe", &providers["stripe"]);

        match manifest.source.auth {
            crate::adapter::manifest::AuthCfg::Header { name, env } => {
                assert_eq!(name, "x-api-key");
                assert_eq!(env, "STRIPE_API_KEY");
            }
            other => panic!("expected header auth, got {other:?}"),
        }
    }

    #[test]
    fn basic_maps_to_basic() {
        let providers = parse_providers(SAMPLE).expect("providers yaml should parse");
        let manifest = provider_to_manifest_stub("github", &providers["github"]);

        match manifest.source.auth {
            crate::adapter::manifest::AuthCfg::Basic { user_env, pass_env } => {
                assert_eq!(user_env, "GITHUB_USER");
                assert_eq!(pass_env, "GITHUB_PASS");
            }
            other => panic!("expected basic auth, got {other:?}"),
        }
    }

    #[test]
    fn oauth_maps_to_bearer_byo() {
        let providers = parse_providers(SAMPLE).expect("providers yaml should parse");
        let manifest = provider_to_manifest_stub("salesforce", &providers["salesforce"]);

        match manifest.source.auth {
            crate::adapter::manifest::AuthCfg::Bearer { env } => {
                assert_eq!(env, "SALESFORCE_TOKEN");
            }
            other => panic!("expected bearer auth, got {other:?}"),
        }

        let pagination = manifest.source.pagination.expect("pagination");
        assert_eq!(pagination.style, "cursor");
        assert_eq!(pagination.cursor_path.as_deref(), Some("$.next_cursor"));
        assert_eq!(pagination.cursor_param.as_deref(), Some("cursor"));
    }

    #[test]
    fn oauth2_with_token_url_maps_to_refresh_grant() {
        let providers = parse_providers(
            r#"
notion:
  display_name: Notion
  auth_mode: OAUTH2
  token_url: "https://api.notion.com/v1/oauth/token"
  proxy:
    base_url: "https://api.notion.com/v1"
"#,
        )
        .expect("providers yaml should parse");
        let manifest = provider_to_manifest_stub("notion", &providers["notion"]);
        match manifest.source.auth {
            crate::adapter::manifest::AuthCfg::Oauth2Refresh {
                token_url,
                client_id_env,
                client_secret_env,
                refresh_token_env,
                ..
            } => {
                assert_eq!(token_url, "https://api.notion.com/v1/oauth/token");
                assert_eq!(client_id_env, "NOTION_CLIENT_ID");
                assert_eq!(client_secret_env, "NOTION_CLIENT_SECRET");
                assert_eq!(refresh_token_env, "NOTION_REFRESH_TOKEN");
            }
            other => panic!("expected oauth2_refresh, got {other:?}"),
        }
    }

    #[test]
    fn oauth2_without_token_url_stays_bearer() {
        let providers = parse_providers(
            r#"
legacy:
  display_name: Legacy
  auth_mode: OAUTH2
  proxy:
    base_url: "https://api.legacy.test"
"#,
        )
        .expect("providers yaml should parse");
        let manifest = provider_to_manifest_stub("legacy", &providers["legacy"]);
        assert!(matches!(
            manifest.source.auth,
            crate::adapter::manifest::AuthCfg::Bearer { .. }
        ));
    }

    #[test]
    fn parses_authorization_code_fields() {
        const Y: &str = r#"
acme:
  display_name: Acme
  auth_mode: OAUTH2
  authorization_url: "https://acme.test/oauth/authorize"
  token_url: "https://acme.test/oauth/token"
  authorization_params:
    response_type: code
    prompt: consent
  scope_separator: " "
  proxy:
    base_url: "https://api.acme.test"
"#;
        let providers = parse_providers(Y).expect("providers yaml should parse");
        let p = &providers["acme"];
        assert_eq!(
            p.authorization_url.as_deref(),
            Some("https://acme.test/oauth/authorize")
        );
        assert_eq!(p.scope_separator.as_deref(), Some(" "));
        let params = p
            .authorization_params
            .as_ref()
            .expect("authorization_params should be present");
        assert_eq!(
            params.get("response_type").map(String::as_str),
            Some("code")
        );
        assert_eq!(params.get("prompt").map(String::as_str), Some("consent"));
    }

    #[test]
    fn google_ads_snapshot_has_authorization_url() {
        const SNAPSHOT: &str =
            include_str!("../../../jute-notebook/src-tauri/src/nango_providers_snapshot.yaml");
        let providers = parse_providers(SNAPSHOT).expect("parse snapshot");
        let g = providers.get("google-ads").expect("google-ads present");
        assert_eq!(
            g.authorization_url.as_deref(),
            Some("https://accounts.google.com/o/oauth2/v2/auth")
        );
        assert!(g
            .authorization_params
            .as_ref()
            .is_some_and(|p| p.contains_key("access_type")));
    }

    #[test]
    fn toml_roundtrips() {
        let providers = parse_providers(SAMPLE).expect("providers yaml should parse");
        let manifest = provider_to_manifest_stub("stripe", &providers["stripe"]);
        let toml = manifest_to_toml(&manifest);

        let reparsed = crate::adapter::manifest::Manifest::from_toml(&toml)
            .expect("generated toml should parse");

        assert_eq!(reparsed.source.name, "stripe");
        assert_eq!(reparsed.tables.len(), 0);
    }

    #[test]
    fn tier_classifies() {
        assert_eq!(tier(Some("API_KEY")), 'A');
        assert_eq!(tier(Some("OAUTH2")), 'B');
    }
}
