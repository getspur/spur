use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{Emitter, Manager};

use super::{check_response, daemon_unavailable, parse_no_args};
use crate::{
    connection_store::ConnectionTemplate,
    mcp::{DaemonControlRequest, DaemonControlResponse, ServerDeps},
};

const LIST_API_PROVIDERS_METHOD: &str = "notebook_list_api_providers";
const PREVIEW_API_TABLES_METHOD: &str = "notebook_preview_api_tables";
const ADD_API_CONNECTION_METHOD: &str = "notebook_add_api_connection";
const LIST_API_CONNECTIONS_METHOD: &str = "notebook_list_api_connections";
const API_CONNECTION_STATUS_METHOD: &str = "notebook_api_connection_status";
pub const OAUTH_CONNECT_METHOD: &str = "notebook.oauth_connect";
const OPEN_REST_WIZARD_EVENT: &str = "notebook://open_rest_wizard";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewApiTablesParams {
    spec_text: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AddApiConnectionParams {
    name: String,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    spec_text: Option<String>,
    #[serde(default)]
    manifest_toml: Option<String>,
    #[serde(default)]
    connection_only: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiConnectionStatusParams {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OauthConnectParams {
    name: String,
}

struct PreparedManifest {
    manifest_toml: String,
    required_env_vars: Vec<String>,
}

fn daemon_request(command: jute::commands::DaemonControlCommand) -> DaemonControlRequest {
    DaemonControlRequest {
        id: None,
        request: jute::commands::DaemonControlRequest::new(command),
    }
}

pub fn list_api_providers_tool() -> Tool {
    Tool::new(
        LIST_API_PROVIDERS_METHOD,
        "List REST API providers available for notebook API connections.",
        rmcp_object(json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })),
    )
}

pub fn preview_api_tables_tool() -> Tool {
    Tool::new(
        PREVIEW_API_TABLES_METHOD,
        "Preview table functions generated from an OpenAPI document.",
        rmcp_object(json!({
            "type": "object",
            "required": ["spec_text"],
            "properties": {
                "spec_text": { "type": "string", "minLength": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub fn add_api_connection_tool() -> Tool {
    Tool::new(
        ADD_API_CONNECTION_METHOD,
        "Add a REST API connection to the active notebook without accepting credential values.",
        rmcp_object(json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": { "type": "string", "minLength": 1 },
                "provider": { "type": "string", "minLength": 1 },
                "spec_text": { "type": "string", "minLength": 1 },
                "manifest_toml": { "type": "string", "minLength": 1 },
                "connection_only": { "type": "boolean" }
            },
            "additionalProperties": false
        })),
    )
}

pub fn list_api_connections_tool() -> Tool {
    Tool::new(
        LIST_API_CONNECTIONS_METHOD,
        "List saved REST API connections and their callable table-function names.",
        rmcp_object(json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })),
    )
}

pub fn api_connection_status_tool() -> Tool {
    Tool::new(
        API_CONNECTION_STATUS_METHOD,
        "Return one saved REST API connection status and callable table-function names.",
        rmcp_object(json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": { "type": "string", "minLength": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub fn oauth_connect_tool() -> Tool {
    Tool::new(
        OAUTH_CONNECT_METHOD,
        "Complete browser OAuth authorization for a saved notebook API connection.",
        rmcp_object(json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": { "type": "string", "minLength": 1 }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call_list_api_providers(
    deps: &ServerDeps,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    parse_no_args(LIST_API_PROVIDERS_METHOD, arguments)?;
    let daemon = deps.daemon.as_ref().ok_or_else(daemon_unavailable)?;
    let response = daemon
        .handle(daemon_request(
            jute::commands::DaemonControlCommand::ListNangoProviders {},
        ))
        .await;
    let response = check_response(response)?;
    let result = daemon_result(LIST_API_PROVIDERS_METHOD, response)?;
    let providers = match result {
        jute::commands::DaemonControlResult::NangoProviders(providers) => providers,
        result => return Err(unexpected_result(LIST_API_PROVIDERS_METHOD, result)),
    };

    Ok(CallToolResult::structured(
        json!({ "providers": providers }),
    ))
}

pub async fn call_preview_api_tables(
    deps: &ServerDeps,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    let params: PreviewApiTablesParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            format!("{PREVIEW_API_TABLES_METHOD} requires {{ spec_text }}"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let daemon = deps.daemon.as_ref().ok_or_else(daemon_unavailable)?;
    let response = daemon
        .handle(daemon_request(
            jute::commands::DaemonControlCommand::PreviewOpenApiTables {
                spec_text: params.spec_text,
            },
        ))
        .await;
    let response = check_response(response)?;
    let result = daemon_result(PREVIEW_API_TABLES_METHOD, response)?;
    let preview = match result {
        jute::commands::DaemonControlResult::OpenApiTablePreview(preview) => preview,
        result => return Err(unexpected_result(PREVIEW_API_TABLES_METHOD, result)),
    };

    Ok(CallToolResult::structured(json!({ "preview": preview })))
}

pub async fn call_add_api_connection(
    deps: &ServerDeps,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    let params: AddApiConnectionParams =
        serde_json::from_value(arguments).map_err(|error| {
            McpError::invalid_params(
                format!(
                    "{ADD_API_CONNECTION_METHOD} requires {{ name, provider?, spec_text?, manifest_toml?, connection_only? }}"
                ),
                Some(json!({ "error": error.to_string() })),
            )
        })?;
    let prepared = prepare_manifest(&params)?;
    let missing_env_vars = missing_env_vars(&prepared.required_env_vars);
    if !missing_env_vars.is_empty() {
        if let Some(action) = oauth_action_for(&prepared) {
            let daemon = deps.daemon.as_ref().ok_or_else(daemon_unavailable)?;
            let response = daemon
                .handle(daemon_request(
                    jute::commands::DaemonControlCommand::SaveApiConnectionTemplate {
                        name: params.name.clone(),
                        provider: params.provider.clone(),
                        manifest_toml: prepared.manifest_toml.clone(),
                    },
                ))
                .await;
            check_response(response)?;

            return Ok(CallToolResult::structured(json!({
                "status": "awaiting_oauth",
                "name": params.name,
                "action": action,
                "tool": OAUTH_CONNECT_METHOD,
                "message": "Click Connect with browser to authorize this connection."
            })));
        }

        let wizard_opened =
            open_rest_wizard(deps, &params, &prepared.manifest_toml, &missing_env_vars);
        return Ok(CallToolResult::structured(json!({
            "status": "awaiting_credentials",
            "name": params.name,
            "missing_env_vars": missing_env_vars,
            "wizard_opened": wizard_opened,
            "message": "Open the REST API wizard to provide credentials before adding this connection."
        })));
    }

    let daemon = deps.daemon.as_ref().ok_or_else(daemon_unavailable)?;
    let response = daemon
        .handle(daemon_request(
            jute::commands::DaemonControlCommand::AddApiDatasourceFromManifest {
                name: params.name,
                manifest_toml: prepared.manifest_toml,
                credentials: Vec::new(),
            },
        ))
        .await;
    let response = check_response(response)?;
    let result = daemon_result(ADD_API_CONNECTION_METHOD, response)?;
    let entry = match result {
        jute::commands::DaemonControlResult::Datasource(entry) => entry,
        result => return Err(unexpected_result(ADD_API_CONNECTION_METHOD, result)),
    };
    let table_functions = table_functions_from_tables(&entry.tables);

    Ok(CallToolResult::structured(json!({
        "status": "ready",
        "entry": entry,
        "table_functions": table_functions,
        "callable_table_functions": table_functions
    })))
}

pub async fn call_list_api_connections(
    deps: &ServerDeps,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    parse_no_args(LIST_API_CONNECTIONS_METHOD, arguments)?;
    let templates = saved_connections(deps, LIST_API_CONNECTIONS_METHOD).await?;
    let connections = templates
        .iter()
        .map(enriched_connection)
        .collect::<Result<Vec<_>, _>>()?;

    Ok(CallToolResult::structured(json!({
        "connections": connections
    })))
}

pub async fn call_api_connection_status(
    deps: &ServerDeps,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    let params: ApiConnectionStatusParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            format!("{API_CONNECTION_STATUS_METHOD} requires {{ name }}"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let templates = saved_connections(deps, API_CONNECTION_STATUS_METHOD).await?;
    let Some(template) = templates
        .iter()
        .find(|template| template.name == params.name)
    else {
        return Ok(CallToolResult::structured(json!({
            "status": "not_found",
            "name": params.name,
            "table_functions": [],
            "callable_table_functions": []
        })));
    };
    let connection = enriched_connection(template)?;

    Ok(CallToolResult::structured(json!({
        "status": connection["status"],
        "name": template.name,
        "connection": connection,
        "table_functions": connection["table_functions"],
        "callable_table_functions": connection["callable_table_functions"]
    })))
}

pub async fn call_oauth_connect(
    deps: &ServerDeps,
    arguments: Value,
) -> Result<CallToolResult, McpError> {
    let params: OauthConnectParams = serde_json::from_value(arguments).map_err(|error| {
        McpError::invalid_params(
            format!("{OAUTH_CONNECT_METHOD} requires {{ name }}"),
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let daemon = deps.daemon.as_ref().ok_or_else(daemon_unavailable)?;
    let response = daemon
        .handle(daemon_request(
            jute::commands::DaemonControlCommand::OauthConnect { name: params.name },
        ))
        .await;
    let response = check_response(response)?;
    let _ = daemon_result(OAUTH_CONNECT_METHOD, response)?;

    Ok(CallToolResult::structured(json!({ "status": "ready" })))
}

async fn saved_connections(
    deps: &ServerDeps,
    method: &str,
) -> Result<Vec<ConnectionTemplate>, McpError> {
    let daemon = deps.daemon.as_ref().ok_or_else(daemon_unavailable)?;
    let response = daemon
        .handle(daemon_request(
            jute::commands::DaemonControlCommand::ListSavedConnections {},
        ))
        .await;
    let response = check_response(response)?;
    let result = daemon_result(method, response)?;
    let payload = match result {
        jute::commands::DaemonControlResult::SavedConnections(payload) => payload,
        result => return Err(unexpected_result(method, result)),
    };
    serde_json::from_value(payload).map_err(|error| {
        McpError::internal_error(
            format!("{method} daemon saved connections did not decode"),
            Some(json!({
                "code": "daemon_result_decode_failed",
                "error": error.to_string()
            })),
        )
    })
}

fn daemon_result(
    method: &str,
    response: DaemonControlResponse,
) -> Result<jute::commands::DaemonControlResult, McpError> {
    let result = response.result.ok_or_else(|| {
        McpError::internal_error(
            format!("{method} daemon response missing result"),
            Some(json!({ "code": "daemon_missing_result" })),
        )
    })?;
    serde_json::from_value(result).map_err(|error| {
        McpError::internal_error(
            format!("{method} daemon response did not decode"),
            Some(json!({
                "code": "daemon_result_decode_failed",
                "error": error.to_string()
            })),
        )
    })
}

fn unexpected_result(method: &str, result: jute::commands::DaemonControlResult) -> McpError {
    McpError::internal_error(
        format!("{method} daemon response returned unexpected result: {result:?}"),
        Some(json!({ "code": "daemon_unexpected_result" })),
    )
}

fn enriched_connection(template: &ConnectionTemplate) -> Result<Value, McpError> {
    let table_functions = table_functions_from_tables(&template.tables);
    let required_env_vars = required_env_vars_for_template(template);
    let missing_env_vars = missing_env_vars(&required_env_vars);
    let status = if missing_env_vars.is_empty() {
        "ready"
    } else {
        "awaiting_credentials"
    };
    let mut value = serde_json::to_value(template).map_err(|error| {
        McpError::internal_error(
            "saved connection did not encode",
            Some(json!({ "error": error.to_string() })),
        )
    })?;
    let Some(object) = value.as_object_mut() else {
        return Err(McpError::internal_error(
            "saved connection encoded to non-object",
            Some(json!({ "code": "saved_connection_encode_failed" })),
        ));
    };
    object.insert("status".to_string(), json!(status));
    object.insert("required_env_vars".to_string(), json!(required_env_vars));
    object.insert("missing_env_vars".to_string(), json!(missing_env_vars));
    object.insert("table_functions".to_string(), json!(table_functions));
    object.insert(
        "callable_table_functions".to_string(),
        json!(table_functions),
    );
    Ok(value)
}

fn table_functions_from_tables(tables: &[jute::commands::Table]) -> Vec<String> {
    tables.iter().map(|table| table.name.clone()).collect()
}

fn missing_env_vars(required_env_vars: &[String]) -> Vec<String> {
    required_env_vars
        .iter()
        .filter(|env_var| std::env::var_os(env_var).is_none())
        .cloned()
        .collect()
}

#[cfg(feature = "datasource-introspect")]
fn oauth_action_for(prepared: &PreparedManifest) -> Option<String> {
    use spur_rest_table_gateway::adapter::manifest::{AuthCfg, Manifest};

    let manifest = Manifest::from_toml(&prepared.manifest_toml).ok()?;
    let AuthCfg::Oauth2Refresh {
        refresh_token_env, ..
    } = manifest.source.auth
    else {
        return None;
    };
    let missing = missing_env_vars(&prepared.required_env_vars);
    if missing.len() == 1 && missing[0] == refresh_token_env {
        Some("connect_with_browser".to_owned())
    } else {
        None
    }
}

#[cfg(not(feature = "datasource-introspect"))]
fn oauth_action_for(_prepared: &PreparedManifest) -> Option<String> {
    None
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

#[cfg(feature = "datasource-introspect")]
fn prepare_manifest(params: &AddApiConnectionParams) -> Result<PreparedManifest, McpError> {
    let manifest_toml = match params
        .manifest_toml
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(manifest_toml) => manifest_toml.to_string(),
        None => {
            let provider = params
                .provider
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned);
            let spec_text = if params.connection_only.unwrap_or(false) {
                None
            } else {
                params
                    .spec_text
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            };
            let (_source, manifest) =
                crate::mcp::build_api_import_manifest(&params.name, provider, spec_text).map_err(
                    |error| {
                        McpError::internal_error(
                            format!("{ADD_API_CONNECTION_METHOD} failed to build API manifest"),
                            Some(json!({ "error": error.to_string() })),
                        )
                    },
                )?;
            let mut manifest_toml =
                spur_rest_table_gateway::adapter::nango::manifest_to_toml(&manifest);
            manifest_toml.push_str(&spur_rest_table_gateway::adapter::openapi::tables_to_toml(
                &manifest.tables,
            ));
            manifest_toml
        }
    };
    let manifest = spur_rest_table_gateway::adapter::manifest::Manifest::from_toml(&manifest_toml)
        .map_err(|error| {
            McpError::invalid_params(
                format!("{ADD_API_CONNECTION_METHOD} manifest_toml did not parse"),
                Some(json!({ "error": error.to_string() })),
            )
        })?;
    let required_env_vars = required_env_vars_from_manifest(&manifest);

    Ok(PreparedManifest {
        manifest_toml,
        required_env_vars,
    })
}

#[cfg(not(feature = "datasource-introspect"))]
fn prepare_manifest(_params: &AddApiConnectionParams) -> Result<PreparedManifest, McpError> {
    Err(McpError::internal_error(
        "datasource introspection is disabled",
        Some(json!({ "code": "datasource_introspect_unavailable" })),
    ))
}

#[cfg(feature = "datasource-introspect")]
fn required_env_vars_for_template(template: &ConnectionTemplate) -> Vec<String> {
    let mut values = template.credential_env_vars.clone();
    if let Ok(manifest) =
        spur_rest_table_gateway::adapter::manifest::Manifest::from_toml(&template.manifest_toml)
    {
        for env_var in required_env_vars_from_manifest(&manifest) {
            push_unique(&mut values, env_var);
        }
    }
    values
}

#[cfg(not(feature = "datasource-introspect"))]
fn required_env_vars_for_template(template: &ConnectionTemplate) -> Vec<String> {
    template.credential_env_vars.clone()
}

#[cfg(feature = "datasource-introspect")]
fn required_env_vars_from_manifest(
    manifest: &spur_rest_table_gateway::adapter::manifest::Manifest,
) -> Vec<String> {
    use spur_rest_table_gateway::adapter::manifest::AuthCfg;

    let mut values = Vec::new();
    match &manifest.source.auth {
        AuthCfg::None => {}
        AuthCfg::Bearer { env }
        | AuthCfg::Header { env, .. }
        | AuthCfg::ApiKeyQuery { env, .. } => {
            push_unique(&mut values, env.clone());
        }
        AuthCfg::Basic { user_env, pass_env } => {
            push_unique(&mut values, user_env.clone());
            push_unique(&mut values, pass_env.clone());
        }
        AuthCfg::Oauth2Refresh {
            client_id_env,
            client_secret_env,
            refresh_token_env,
            ..
        } => {
            push_unique(&mut values, client_id_env.clone());
            push_unique(&mut values, client_secret_env.clone());
            push_unique(&mut values, refresh_token_env.clone());
        }
    }
    for name in &manifest.source.connection_config {
        push_unique(&mut values, format!("SPUR_CONN_{name}"));
    }
    values
}

fn open_rest_wizard(
    deps: &ServerDeps,
    params: &AddApiConnectionParams,
    manifest_toml: &str,
    missing_env_vars: &[String],
) -> bool {
    let Some(app) = deps.app.as_ref() else {
        return false;
    };
    let emitted = app
        .emit(
            OPEN_REST_WIZARD_EVENT,
            json!({
                "name": params.name,
                "provider": params.provider,
                "spec_text": params.spec_text,
                "manifest_toml": manifest_toml,
                "connection_only": params.connection_only.unwrap_or(false),
                "missing_env_vars": missing_env_vars
            }),
        )
        .is_ok();
    if let Some((_label, window)) = app.webview_windows().into_iter().next() {
        let _ = window.show();
        let _ = window.set_focus();
    }
    emitted
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct EnvVarGuard {
        key: String,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: impl Into<String>, value: &str) -> Self {
            let key = key.into();
            let previous = std::env::var(&key).ok();
            std::env::set_var(&key, value);
            Self { key, previous }
        }

        fn unset(key: impl Into<String>) -> Self {
            let key = key.into();
            let previous = std::env::var(&key).ok();
            std::env::remove_var(&key);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                std::env::set_var(&self.key, previous);
            } else {
                std::env::remove_var(&self.key);
            }
        }
    }

    fn schema(tool: Tool) -> Value {
        Value::Object((*tool.input_schema).clone())
    }

    #[test]
    fn api_connection_tool_names_are_exact() {
        let names = vec![
            list_api_providers_tool().name.to_string(),
            preview_api_tables_tool().name.to_string(),
            add_api_connection_tool().name.to_string(),
            list_api_connections_tool().name.to_string(),
            api_connection_status_tool().name.to_string(),
            oauth_connect_tool().name.to_string(),
        ];

        assert_eq!(
            names,
            vec![
                "notebook_list_api_providers",
                "notebook_preview_api_tables",
                "notebook_add_api_connection",
                "notebook_list_api_connections",
                "notebook_api_connection_status",
                "notebook.oauth_connect"
            ]
        );
    }

    #[test]
    fn add_api_connection_schema_has_no_credentials_field() {
        let schema = schema(add_api_connection_tool());
        let properties = schema["properties"].as_object().expect("properties object");

        assert!(properties.contains_key("name"));
        assert!(properties.contains_key("provider"));
        assert!(properties.contains_key("spec_text"));
        assert!(properties.contains_key("manifest_toml"));
        assert!(properties.contains_key("connection_only"));
        assert!(!properties.contains_key("credentials"));
        assert_eq!(schema["additionalProperties"], json!(false));
    }

    #[test]
    fn add_api_connection_params_reject_credentials() {
        let error = serde_json::from_value::<AddApiConnectionParams>(json!({
            "name": "stripe",
            "credentials": [["STRIPE_API_KEY", "secret"]]
        }))
        .expect_err("credentials must not deserialize");

        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn no_arg_tool_schemas_are_closed_objects() {
        for schema in [
            schema(list_api_providers_tool()),
            schema(list_api_connections_tool()),
        ] {
            assert_eq!(schema["type"], json!("object"));
            assert_eq!(schema["properties"], json!({}));
            assert_eq!(schema["additionalProperties"], json!(false));
        }
    }

    #[test]
    fn preview_and_status_schemas_require_expected_fields() {
        let preview = schema(preview_api_tables_tool());
        assert_eq!(preview["required"], json!(["spec_text"]));
        assert!(preview["properties"].get("credentials").is_none());

        let status = schema(api_connection_status_tool());
        assert_eq!(status["required"], json!(["name"]));
        assert!(status["properties"].get("credentials").is_none());

        let oauth = schema(oauth_connect_tool());
        assert_eq!(oauth["required"], json!(["name"]));
        assert!(oauth["properties"].get("credentials").is_none());
    }

    #[test]
    fn oauth_only_missing_refresh_token_returns_connect_with_browser() {
        let _env_lock = env_lock();
        let _cid = EnvVarGuard::set("GOOGLE_ADS_CLIENT_ID", "x");
        let _sec = EnvVarGuard::set("GOOGLE_ADS_CLIENT_SECRET", "y");
        let _refresh = EnvVarGuard::unset("GOOGLE_ADS_REFRESH_TOKEN");
        let _dev = EnvVarGuard::set("SPUR_CONN_DEVELOPER_TOKEN", "dev");
        let _login = EnvVarGuard::set("SPUR_CONN_LOGIN_CUSTOMER_ID", "123");
        let prepared = prepare_manifest(&AddApiConnectionParams {
            name: "google_ads".into(),
            provider: Some("google-ads".into()),
            spec_text: None,
            manifest_toml: None,
            connection_only: None,
        })
        .expect("prepare");

        let action = oauth_action_for(&prepared);

        assert_eq!(action.as_deref(), Some("connect_with_browser"));
    }

    #[test]
    fn oauth_action_none_when_non_oauth_var_also_missing() {
        let _env_lock = env_lock();
        let _cid = EnvVarGuard::set("GOOGLE_ADS_CLIENT_ID", "x");
        let _sec = EnvVarGuard::set("GOOGLE_ADS_CLIENT_SECRET", "y");
        let _refresh = EnvVarGuard::unset("GOOGLE_ADS_REFRESH_TOKEN");
        let _dev = EnvVarGuard::unset("SPUR_CONN_DEVELOPER_TOKEN");
        let _login = EnvVarGuard::unset("SPUR_CONN_LOGIN_CUSTOMER_ID");
        let prepared = prepare_manifest(&AddApiConnectionParams {
            name: "google_ads".into(),
            provider: Some("google-ads".into()),
            spec_text: None,
            manifest_toml: None,
            connection_only: None,
        })
        .expect("prepare");

        assert_eq!(oauth_action_for(&prepared), None);
    }

    #[test]
    fn enriched_connection_returns_exact_callable_table_function_names() {
        let template = ConnectionTemplate {
            name: "stripe_reporting".to_string(),
            provider: Some("stripe".to_string()),
            group: Some("API".to_string()),
            manifest_toml: r#"
[source]
name = "stripe"
base_url = "https://api.stripe.com"
auth = { scheme = "bearer", env = "STRIPE_API_KEY" }

[[table]]
name = "charges"
path = "/charges"

[table.columns]
id = { json = "$.id", type = "Utf8" }
"#
            .to_string(),
            tables: vec![jute::commands::Table {
                name: "stripe_charges".to_string(),
                columns: Vec::new(),
                row_count: None,
            }],
            credential_env_vars: vec!["STRIPE_API_KEY".to_string()],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let enriched = enriched_connection(&template).expect("connection enriches");

        assert_eq!(enriched["table_functions"], json!(["stripe_charges"]));
        assert_eq!(
            enriched["callable_table_functions"],
            json!(["stripe_charges"])
        );
    }
}
