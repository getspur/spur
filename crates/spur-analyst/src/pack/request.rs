use serde_json::Value;

use crate::mcp::McpHandlerError;
use crate::{
    KnowledgePathOptions, KnowledgeQueryIntent, KnowledgeSearchScope, MAX_CONTEXT_PATHS,
    MAX_CONTEXT_PATH_HOPS,
};

#[derive(Clone, Debug)]
pub(crate) struct KnowledgeContextPackRequest {
    pub(crate) query: String,
    pub(crate) intent: KnowledgeIntent,
    pub(crate) scope: KnowledgeScope,
    pub(crate) limit: u64,
    pub(crate) include_tests: bool,
    pub(crate) max_symbol_bodies: u64,
}

impl KnowledgeContextPackRequest {
    pub(crate) fn parse(args: &Value) -> Result<Self, McpHandlerError> {
        let query = parse_query(args)?;
        let intent = KnowledgeIntent::parse(parse_enum(
            args,
            "intent",
            &["explain", "change", "review", "debug", "plan"],
            "explain",
        )?);
        let scope = KnowledgeScope::parse(parse_enum(
            args,
            "scope",
            &["all", "docs", "code", "graph"],
            "all",
        )?);
        let limit = parse_u64(args, "limit", 8, 1, 20)?;
        let include_tests = parse_include_tests(args)?;
        let max_symbol_bodies = parse_u64(args, "max_symbol_bodies", 0, 0, 5)?;

        Ok(Self {
            query,
            intent,
            scope,
            limit,
            include_tests,
            max_symbol_bodies,
        })
    }

    pub(crate) fn should_query_graph_candidates(&self) -> bool {
        matches!(self.scope, KnowledgeScope::Graph)
            || (matches!(self.scope, KnowledgeScope::All)
                && matches!(
                    self.intent,
                    KnowledgeIntent::Debug | KnowledgeIntent::Change
                ))
    }
}

fn parse_query(args: &Value) -> Result<String, McpHandlerError> {
    args.get("query")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            McpHandlerError::InvalidParams(
                "knowledge_context_pack requires non-empty string field 'query'".into(),
            )
        })
}

fn parse_include_tests(args: &Value) -> Result<bool, McpHandlerError> {
    args.get("include_tests")
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                McpHandlerError::InvalidParams(
                    "knowledge_context_pack field 'include_tests' must be a boolean".into(),
                )
            })
        })
        .transpose()
        .map(|value| value.unwrap_or(true))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PackResponseFormat {
    Compact,
    Full,
}

impl PackResponseFormat {
    fn parse(args: &Value) -> Result<Self, McpHandlerError> {
        match parse_enum(args, "response_format", &["full", "compact"], "compact")?.as_str() {
            "full" => Ok(Self::Full),
            _ => Ok(Self::Compact),
        }
    }

    pub(crate) fn is_compact(self) -> bool {
        matches!(self, Self::Compact)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct KnowledgeContextPackV2Request {
    pub(crate) base: KnowledgeContextPackRequest,
    pub(crate) graph_reasoning: GraphReasoningOptions,
    pub(crate) response_format: PackResponseFormat,
}

impl KnowledgeContextPackV2Request {
    pub(crate) fn parse(args: &Value) -> Result<Self, McpHandlerError> {
        let base = KnowledgeContextPackRequest::parse(args)?;
        let graph_reasoning = GraphReasoningOptions::parse(args, base.intent, base.scope)?;
        let response_format = PackResponseFormat::parse(args)?;
        Ok(Self {
            base,
            graph_reasoning,
            response_format,
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct GraphReasoningOptions {
    pub(crate) paths: bool,
    pub(crate) communities: bool,
    pub(crate) communities_explicit: bool,
    pub(crate) risk: bool,
    pub(crate) max_path_hops: usize,
    pub(crate) max_paths: usize,
    pub(crate) anchors: Vec<String>,
}

impl GraphReasoningOptions {
    fn parse(
        args: &Value,
        intent: KnowledgeIntent,
        scope: KnowledgeScope,
    ) -> Result<Self, McpHandlerError> {
        let defaults = GraphReasoningDefaults::from(intent, scope);
        let Some(value) = args.get("graph_reasoning") else {
            return Ok(defaults.into_options());
        };
        let object = value.as_object().ok_or_else(|| {
            McpHandlerError::InvalidParams(
                "knowledge_context_pack_2 field 'graph_reasoning' must be an object".into(),
            )
        })?;
        parse_graph_reasoning_object(object, defaults)
    }

    pub(crate) fn should_query_communities(&self, code_symbol_count: usize) -> bool {
        if !self.communities {
            return false;
        }
        self.communities_explicit || code_symbol_count >= 2
    }

    pub(crate) fn any_enabled(&self) -> bool {
        self.paths || self.communities || self.risk
    }
}

struct GraphReasoningDefaults {
    paths: bool,
    risk: bool,
    communities: bool,
    max_path_hops: usize,
    max_paths: usize,
}

impl GraphReasoningDefaults {
    fn from(intent: KnowledgeIntent, scope: KnowledgeScope) -> Self {
        let investigative = matches!(
            intent,
            KnowledgeIntent::Change | KnowledgeIntent::Review | KnowledgeIntent::Debug
        );
        Self {
            paths: investigative,
            risk: investigative && !matches!(scope, KnowledgeScope::Docs),
            communities: investigative,
            max_path_hops: KnowledgePathOptions::default().max_hops,
            max_paths: KnowledgePathOptions::default().max_paths,
        }
    }

    fn into_options(self) -> GraphReasoningOptions {
        GraphReasoningOptions {
            paths: self.paths,
            communities: self.communities,
            communities_explicit: false,
            risk: self.risk,
            max_path_hops: self.max_path_hops,
            max_paths: self.max_paths,
            anchors: Vec::new(),
        }
    }
}

fn parse_graph_reasoning_object(
    object: &serde_json::Map<String, Value>,
    defaults: GraphReasoningDefaults,
) -> Result<GraphReasoningOptions, McpHandlerError> {
    let communities = parse_optional_bool_v2(object, "communities")?;
    Ok(GraphReasoningOptions {
        paths: parse_optional_bool_v2(object, "paths")?.unwrap_or(defaults.paths),
        communities: communities.unwrap_or(defaults.communities),
        communities_explicit: communities.is_some(),
        risk: parse_optional_bool_v2(object, "risk")?.unwrap_or(defaults.risk),
        max_path_hops: parse_clamped_usize_v2(
            object,
            "max_path_hops",
            defaults.max_path_hops,
            1,
            MAX_CONTEXT_PATH_HOPS,
        )?,
        max_paths: parse_clamped_usize_v2(
            object,
            "max_paths",
            defaults.max_paths,
            1,
            MAX_CONTEXT_PATHS,
        )?,
        anchors: parse_anchor_array_v2(object)?,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KnowledgeIntent {
    Explain,
    Change,
    Review,
    Debug,
    Plan,
}

impl KnowledgeIntent {
    fn parse(value: String) -> Self {
        match value.as_str() {
            "change" => Self::Change,
            "review" => Self::Review,
            "debug" => Self::Debug,
            "plan" => Self::Plan,
            _ => Self::Explain,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Explain => "explain",
            Self::Change => "change",
            Self::Review => "review",
            Self::Debug => "debug",
            Self::Plan => "plan",
        }
    }

    pub(crate) fn as_analyst_intent(self) -> KnowledgeQueryIntent {
        match self {
            Self::Explain => KnowledgeQueryIntent::Explain,
            Self::Change => KnowledgeQueryIntent::Change,
            Self::Review => KnowledgeQueryIntent::Review,
            Self::Debug => KnowledgeQueryIntent::Debug,
            Self::Plan => KnowledgeQueryIntent::Plan,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KnowledgeScope {
    All,
    Docs,
    Code,
    Graph,
}

impl KnowledgeScope {
    fn parse(value: String) -> Self {
        match value.as_str() {
            "docs" => Self::Docs,
            "code" => Self::Code,
            "graph" => Self::Graph,
            _ => Self::All,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Docs => "docs",
            Self::Code => "code",
            Self::Graph => "graph",
        }
    }

    pub(crate) fn as_analyst_scope(self) -> KnowledgeSearchScope {
        match self {
            Self::All => KnowledgeSearchScope::All,
            Self::Docs => KnowledgeSearchScope::Docs,
            Self::Code => KnowledgeSearchScope::Code,
            Self::Graph => KnowledgeSearchScope::Graph,
        }
    }
}

fn parse_enum(
    args: &Value,
    field: &str,
    allowed: &[&str],
    default: &str,
) -> Result<String, McpHandlerError> {
    let Some(value) = args.get(field) else {
        return Ok(default.to_owned());
    };
    let value = value.as_str().ok_or_else(|| {
        McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack field '{field}' must be a string"
        ))
    })?;
    if allowed.contains(&value) {
        Ok(value.to_owned())
    } else {
        Err(McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack field '{field}' must be one of {}",
            allowed.join("|")
        )))
    }
}

fn parse_u64(
    args: &Value,
    field: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, McpHandlerError> {
    let Some(value) = args.get(field) else {
        return Ok(default);
    };
    let value = value.as_u64().ok_or_else(|| {
        McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack field '{field}' must be an integer"
        ))
    })?;
    if (min..=max).contains(&value) {
        Ok(value)
    } else {
        Err(McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack field '{field}' must be between {min} and {max}"
        )))
    }
}

fn parse_optional_bool_v2(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<bool>, McpHandlerError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    value.as_bool().map(Some).ok_or_else(|| {
        McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack_2 graph_reasoning field '{field}' must be a boolean"
        ))
    })
}

fn parse_clamped_usize_v2(
    object: &serde_json::Map<String, Value>,
    field: &str,
    default: usize,
    min: usize,
    max: usize,
) -> Result<usize, McpHandlerError> {
    let Some(value) = object.get(field) else {
        return Ok(default);
    };
    let value = value.as_i64().ok_or_else(|| {
        McpHandlerError::InvalidParams(format!(
            "knowledge_context_pack_2 graph_reasoning field '{field}' must be an integer"
        ))
    })?;
    Ok(value.clamp(min as i64, max as i64) as usize)
}

fn parse_anchor_array_v2(
    object: &serde_json::Map<String, Value>,
) -> Result<Vec<String>, McpHandlerError> {
    let Some(value) = object.get("anchors") else {
        return Ok(Vec::new());
    };
    let values = value.as_array().ok_or_else(|| {
        McpHandlerError::InvalidParams(
            "knowledge_context_pack_2 graph_reasoning field 'anchors' must be an array".into(),
        )
    })?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|anchor| !anchor.is_empty())
                .map(str::to_string)
                .ok_or_else(|| {
                    McpHandlerError::InvalidParams(
                        "knowledge_context_pack_2 graph_reasoning field 'anchors' must contain non-empty strings"
                            .into(),
                    )
                })
        })
        .collect()
}

#[cfg(test)]
#[path = "request_tests.rs"]
mod tests;
