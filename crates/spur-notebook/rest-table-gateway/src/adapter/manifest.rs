use indexmap::IndexMap;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub source: SourceCfg,
    #[serde(default, rename = "table")]
    pub tables: Vec<TableCfg>,
}

#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    #[default]
    Rest,
    Graphql,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceCfg {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub transport: Transport,
    #[serde(default)]
    pub auth: AuthCfg,
    pub pagination: Option<PaginationCfg>,
    #[serde(default)]
    pub connection_config: Vec<String>,
}

/// Authentication config uses serde's internally tagged representation.
///
/// In TOML, write `auth = { scheme = "none" }` or omit `auth` to use the
/// default `none` scheme; the shorthand `auth = "none"` is not supported.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "scheme", rename_all = "snake_case")]
pub enum AuthCfg {
    None,
    Bearer {
        env: String,
    },
    Header {
        name: String,
        env: String,
    },
    Basic {
        user_env: String,
        pass_env: String,
    },
    ApiKeyQuery {
        param: String,
        env: String,
    },
    Oauth2Refresh {
        token_url: String,
        client_id_env: String,
        client_secret_env: String,
        refresh_token_env: String,
        #[serde(default)]
        scope: Option<String>,
    },
}

impl Default for AuthCfg {
    fn default() -> Self {
        AuthCfg::None
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaginationCfg {
    pub style: String,
    #[serde(default)]
    pub limit_param: Option<String>,
    #[serde(default)]
    pub offset_param: Option<String>,
    #[serde(default)]
    pub page_size: u32,
    #[serde(default)]
    pub cursor_path: Option<String>,
    #[serde(default)]
    pub cursor_param: Option<String>,
    #[serde(default)]
    pub link_rel: Option<String>,
    #[serde(default)]
    pub has_next_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TableCfg {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub response_path: Option<String>,
    pub columns: IndexMap<String, ColumnCfg>,
    #[serde(default)]
    pub filters: HashMap<String, FilterCfg>,
    #[serde(default)]
    pub graphql: Option<GraphqlTableCfg>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ColumnCfg {
    pub json: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FilterCfg {
    pub param: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GraphqlTableCfg {
    pub query: String,
    #[serde(default)]
    pub variables: serde_json::Value,
    #[serde(default)]
    pub arg_vars: std::collections::HashMap<String, String>,
}

impl Manifest {
    pub fn from_toml(s: &str) -> crate::error::Result<Self> {
        toml::from_str(s).map_err(|e| crate::error::GatewayError::Manifest(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_markets_manifest() {
        let manifest = Manifest::from_toml(
            r#"
[source]
name = "polymarket"
base_url = "https://gamma-api.polymarket.com"
pagination = { style = "offset", limit_param = "limit", offset_param = "offset", page_size = 500 }

[[table]]
name = "markets"
path = "/markets"

[table.columns]
id = { json = "$.id", type = "Utf8" }
question = { json = "$.question", type = "Utf8" }
active = { json = "$.active", type = "Boolean" }
volume = { json = "$.volume", type = "Float64" }

[table.filters]
active = { param = "active" }
"#,
        )
        .expect("manifest should parse");

        assert_eq!(manifest.source.name, "polymarket");
        assert_eq!(manifest.tables.len(), 1);
        assert_eq!(manifest.tables[0].name, "markets");
        assert_eq!(
            manifest.tables[0].columns.keys().collect::<Vec<_>>(),
            vec!["id", "question", "active", "volume"]
        );
        assert_eq!(manifest.tables[0].columns["volume"].ty, "Float64");
        assert_eq!(manifest.tables[0].filters["active"].param, "active");
    }

    #[test]
    fn parses_extended_connection_fields() {
        let manifest = Manifest::from_toml(
            r#"
[source]
name = "nango"
base_url = "https://${connectionConfig.host}"
connection_config = ["host", "workspace"]
auth = { scheme = "basic", user_env = "NANGO_USER", pass_env = "NANGO_PASS" }
pagination = { style = "cursor", page_size = 500, cursor_path = "$.next_cursor", cursor_param = "cursor" }

[[table]]
name = "accounts"
path = "/accounts"
response_path = "$.data"

[table.columns]
id = { json = "$.id", type = "Utf8" }
"#,
        )
        .expect("manifest should parse");

        assert_eq!(manifest.source.connection_config, ["host", "workspace"]);
        match manifest.source.auth {
            AuthCfg::Basic { user_env, pass_env } => {
                assert_eq!(user_env, "NANGO_USER");
                assert_eq!(pass_env, "NANGO_PASS");
            }
            other => panic!("expected basic auth, got {other:?}"),
        }

        let pagination = manifest.source.pagination.expect("pagination");
        assert_eq!(pagination.style, "cursor");
        assert_eq!(pagination.limit_param, None);
        assert_eq!(pagination.offset_param, None);
        assert_eq!(pagination.page_size, 500);
        assert_eq!(pagination.cursor_path.as_deref(), Some("$.next_cursor"));
        assert_eq!(pagination.cursor_param.as_deref(), Some("cursor"));
        assert_eq!(pagination.link_rel, None);
        assert_eq!(manifest.tables[0].response_path.as_deref(), Some("$.data"));
    }
}
