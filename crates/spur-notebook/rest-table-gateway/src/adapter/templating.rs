use std::collections::HashMap;

use crate::error::{GatewayError, Result};

#[derive(Debug, Clone, Default)]
pub struct ConnectionContext {
    pub params: HashMap<String, String>,
}

impl ConnectionContext {
    pub fn from_env(names: &[String]) -> Self {
        let params = names
            .iter()
            .filter_map(|name| {
                std::env::var(format!("SPUR_CONN_{name}"))
                    .ok()
                    .map(|value| (name.clone(), value))
            })
            .collect();

        Self { params }
    }
}

pub fn resolve_template(input: &str, ctx: &ConnectionContext) -> Result<String> {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    let marker = "${connectionConfig.";

    while let Some(start) = rest.find(marker) {
        output.push_str(&rest[..start]);
        let after_marker = &rest[start + marker.len()..];
        let Some(end) = after_marker.find('}') else {
            output.push_str(&rest[start..]);
            return Ok(output);
        };

        let name = &after_marker[..end];
        let value = ctx.params.get(name).ok_or_else(|| {
            GatewayError::Manifest(format!("unresolved connection param: {name}"))
        })?;
        output.push_str(value);
        rest = &after_marker[end + 1..];
    }

    output.push_str(rest);
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::{resolve_template, ConnectionContext};

    #[test]
    fn resolves_present_param() {
        std::env::set_var("SPUR_CONN_templating_present", "api.example.com");
        let ctx = ConnectionContext::from_env(&["templating_present".to_string()]);

        let resolved =
            resolve_template("https://${connectionConfig.templating_present}/v1", &ctx).unwrap();

        assert_eq!(resolved, "https://api.example.com/v1");
    }

    #[test]
    fn unresolved_is_error() {
        let ctx = ConnectionContext::from_env(&[]);

        let err = resolve_template("${connectionConfig.missing}", &ctx).unwrap_err();

        assert_eq!(
            err.to_string(),
            "manifest error: unresolved connection param: missing"
        );
    }

    #[test]
    fn passthrough() {
        let ctx = ConnectionContext::from_env(&[]);

        let resolved = resolve_template("https://api.example.com/v1", &ctx).unwrap();

        assert_eq!(resolved, "https://api.example.com/v1");
    }
}
