use std::collections::BTreeMap;

use lambda_runtime::Error;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::auth::{self, AuthFailure, IamContext, RequestRoute};

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub(crate) struct CallerIdentityError {
    message: &'static str,
}

impl CallerIdentityError {
    fn authenticated_caller_required() -> Self {
        Self {
            message: "authenticated caller is required for mutating context-service tools",
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiGatewayRequest {
    pub(crate) body: Option<String>,
    #[serde(rename = "isBase64Encoded", default)]
    pub(crate) is_base64_encoded: bool,
    #[serde(default)]
    pub(crate) path: Option<String>,
    #[serde(rename = "rawPath", default)]
    pub(crate) raw_path: Option<String>,
    #[serde(rename = "rawQueryString", default)]
    pub(crate) raw_query_string: Option<String>,
    #[serde(rename = "queryStringParameters", default)]
    pub(crate) query_string_parameters: Option<BTreeMap<String, String>>,
    #[serde(rename = "requestContext", default)]
    pub(crate) request_context: Option<ApiGatewayRequestContext>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiGatewayRequestContext {
    #[serde(default)]
    pub(crate) authorizer: Option<ApiGatewayAuthorizer>,
    #[serde(default)]
    pub(crate) http: Option<ApiGatewayHttp>,
    #[serde(default)]
    pub(crate) identity: Option<ApiGatewayIdentity>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiGatewayAuthorizer {
    #[serde(rename = "principalId", default)]
    pub(crate) principal_id: Option<String>,
    #[serde(default)]
    pub(crate) iam: Option<IamAuthorizer>,
    #[serde(default)]
    pub(crate) jwt: Option<JwtAuthorizer>,
    #[serde(default)]
    pub(crate) lambda: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct IamAuthorizer {
    #[serde(rename = "userArn", default)]
    pub(crate) user_arn: Option<String>,
    #[serde(rename = "callerId", default)]
    pub(crate) caller_id: Option<String>,
    #[serde(rename = "userId", default)]
    pub(crate) user_id: Option<String>,
    #[serde(rename = "accountId", default)]
    pub(crate) account_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct JwtAuthorizer {
    #[serde(default)]
    pub(crate) claims: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiGatewayHttp {
    #[serde(default)]
    pub(crate) method: Option<String>,
    #[serde(rename = "sourceIp", default)]
    pub(crate) source_ip: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiGatewayIdentity {
    #[serde(rename = "userArn", default)]
    pub(crate) user_arn: Option<String>,
    #[serde(rename = "sourceIp", default)]
    pub(crate) source_ip: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct ApiGatewayResponse {
    #[serde(rename = "statusCode")]
    pub(crate) status_code: u16,
    pub(crate) headers: BTreeMap<String, String>,
    pub(crate) body: String,
    #[serde(rename = "isBase64Encoded")]
    pub(crate) is_base64_encoded: bool,
}

pub(crate) fn classify_route(request: &ApiGatewayRequest) -> RequestRoute {
    auth::classify_route(
        request.raw_path.as_deref().or(request.path.as_deref()),
        request
            .request_context
            .as_ref()
            .and_then(|context| context.http.as_ref())
            .and_then(|http| http.method.as_deref()),
    )
}

pub(crate) fn reject_jwt_auth_on_wrong_route(
    request: &ApiGatewayRequest,
) -> Result<(), AuthFailure> {
    let has_jwt_context = request
        .request_context
        .as_ref()
        .and_then(|context| context.authorizer.as_ref())
        .and_then(|authorizer| authorizer.jwt.as_ref())
        .is_some();

    if has_jwt_context
        && !matches!(
            classify_route(request),
            RequestRoute::OAuth
                | RequestRoute::ApiKeyCreate
                | RequestRoute::ApiKeyList
                | RequestRoute::ApiKeyRevoke
        )
    {
        Err(AuthFailure::WrongRoute)
    } else {
        Ok(())
    }
}

pub(crate) fn reject_api_key_auth_on_wrong_route(
    request: &ApiGatewayRequest,
) -> Result<(), AuthFailure> {
    let has_api_key_context = request
        .request_context
        .as_ref()
        .and_then(|context| context.authorizer.as_ref())
        .and_then(|authorizer| authorizer.lambda.as_ref())
        .is_some();
    if has_api_key_context && classify_route(request) != RequestRoute::ApiKeyMcp {
        Err(AuthFailure::WrongRoute)
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn caller_id(request: &ApiGatewayRequest) -> String {
    request
        .request_context
        .as_ref()
        .and_then(|context| {
            context
                .authorizer
                .as_ref()
                .and_then(jwt_caller_id)
                .or_else(|| context.authorizer.as_ref().and_then(iam_caller_id))
                .or_else(|| {
                    context
                        .authorizer
                        .as_ref()
                        .and_then(|authorizer| non_blank(authorizer.principal_id.as_deref()))
                })
                .or_else(|| {
                    context
                        .http
                        .as_ref()
                        .and_then(|http| non_blank(http.source_ip.as_deref()))
                })
                .or_else(|| {
                    context
                        .identity
                        .as_ref()
                        .and_then(|identity| non_blank(identity.user_arn.as_deref()))
                })
                .or_else(|| {
                    context
                        .identity
                        .as_ref()
                        .and_then(|identity| non_blank(identity.source_ip.as_deref()))
                })
        })
        .unwrap_or("anonymous")
        .to_owned()
}

pub(crate) fn authenticated_caller_id(
    request: &ApiGatewayRequest,
    allow_anonymous: bool,
) -> Result<String, CallerIdentityError> {
    let caller = request
        .request_context
        .as_ref()
        .and_then(|context| {
            let authorizer = context.authorizer.as_ref();
            let strict_iam_identity = authorizer.and_then(|authorizer| {
                authorizer.iam.as_ref().and_then(|iam| {
                    IamContext {
                        account_id: iam.account_id.as_deref(),
                        user_id: iam.user_id.as_deref(),
                        user_arn: iam.user_arn.as_deref(),
                    }
                    .authenticate()
                    .ok()
                    .map(|identity| identity.caller_id().to_owned())
                })
            });

            authorizer
                .and_then(jwt_caller_id)
                .map(str::to_owned)
                .or(strict_iam_identity)
                .or_else(|| authorizer.and_then(iam_caller_id).map(str::to_owned))
                .or_else(|| {
                    authorizer
                        .and_then(|authorizer| non_blank(authorizer.principal_id.as_deref()))
                        .map(str::to_owned)
                })
                .or_else(|| {
                    context
                        .identity
                        .as_ref()
                        .and_then(|identity| non_blank(identity.user_arn.as_deref()))
                        .map(str::to_owned)
                })
        })
        .or_else(|| {
            allow_anonymous.then(|| auth::legacy_anonymous_identity().caller_id().to_owned())
        });

    caller.ok_or_else(CallerIdentityError::authenticated_caller_required)
}

fn jwt_caller_id(authorizer: &ApiGatewayAuthorizer) -> Option<&str> {
    let claims = authorizer.jwt.as_ref()?.claims.as_ref()?;
    claim_str(claims, "sub").or_else(|| claim_str(claims, "principal_id"))
}

fn iam_caller_id(authorizer: &ApiGatewayAuthorizer) -> Option<&str> {
    let iam = authorizer.iam.as_ref()?;
    non_blank(iam.user_arn.as_deref())
        .or_else(|| non_blank(iam.caller_id.as_deref()))
        .or_else(|| non_blank(iam.user_id.as_deref()))
        .or_else(|| non_blank(iam.account_id.as_deref()))
}

fn claim_str<'a>(claims: &'a Value, key: &str) -> Option<&'a str> {
    non_blank(claims.get(key).and_then(Value::as_str))
}

fn non_blank(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

pub(crate) fn json_response(status_code: u16, value: &Value) -> Result<ApiGatewayResponse, Error> {
    Ok(ApiGatewayResponse {
        status_code,
        headers: json_headers(),
        body: serde_json::to_string(value).map_err(Error::from)?,
        is_base64_encoded: false,
    })
}

fn json_headers() -> BTreeMap<String, String> {
    BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())])
}
