use indexmap::IndexMap;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize)]
pub struct Manifest {
    pub source: SourceCfg,
    #[serde(default, rename = "table")]
    pub tables: Vec<TableCfg>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceCfg {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub auth: AuthCfg,
    pub pagination: Option<PaginationCfg>,
}

/// Authentication config uses serde's internally tagged representation.
///
/// In TOML, write `auth = { scheme = "none" }` or omit `auth` to use the
/// default `none` scheme; the shorthand `auth = "none"` is not supported.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "scheme", rename_all = "snake_case")]
pub enum AuthCfg {
    None,
    Bearer { env: String },
    Header { name: String, env: String },
}

impl Default for AuthCfg {
    fn default() -> Self {
        AuthCfg::None
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct PaginationCfg {
    pub style: String,
    pub limit_param: String,
    pub offset_param: String,
    pub page_size: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TableCfg {
    pub name: String,
    pub path: String,
    pub columns: IndexMap<String, ColumnCfg>,
    #[serde(default)]
    pub filters: HashMap<String, FilterCfg>,
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
}
