//! Progressive-disclosure queries over the built-in rule registry.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use thiserror::Error;

use super::{builtin_registry, catalog::RuleDefinition};

/// Detail section included in a rule catalog response.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSpecInclude {
    /// Stable rule identity, applicability, and authority metadata.
    #[default]
    Summary,
    /// One fixture that satisfies the rule.
    ValidExample,
    /// One fixture that violates the rule.
    InvalidExample,
    /// Agent-facing recognition and encoding instructions.
    LlmEncoding,
    /// Solver theory, formula, and proof strategy.
    SolverEncoding,
    /// Every available guidance section.
    All,
}

/// A bounded query over the static rule catalog.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RuleSpecRequest {
    /// Exact family ID.
    pub family: Option<String>,
    /// Exact profile ID.
    pub profile: Option<String>,
    /// Exact rule ID.
    pub rule_id: Option<String>,
    /// Exact primitive name; may match more than one rule.
    pub primitive: Option<String>,
    /// Optional detail section, defaulting to summary cards.
    #[serde(default)]
    pub include: RuleSpecInclude,
}

impl RuleSpecRequest {
    fn selector(&self) -> Result<RuleSpecSelector<'_>, RuleSpecError> {
        let selectors = [
            self.family.as_deref().map(RuleSpecSelector::Family),
            self.profile.as_deref().map(RuleSpecSelector::Profile),
            self.rule_id.as_deref().map(RuleSpecSelector::RuleId),
            self.primitive.as_deref().map(RuleSpecSelector::Primitive),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

        match selectors.as_slice() {
            [] => Ok(RuleSpecSelector::Catalog),
            [selector] => Ok(*selector),
            _ => Err(RuleSpecError::AmbiguousSelector),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum RuleSpecSelector<'a> {
    Catalog,
    Family(&'a str),
    Profile(&'a str),
    RuleId(&'a str),
    Primitive(&'a str),
}

impl RuleSpecSelector<'_> {
    const fn name(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Family(_) => "family",
            Self::Profile(_) => "profile",
            Self::RuleId(_) => "rule_id",
            Self::Primitive(_) => "primitive",
        }
    }

    const fn value(&self) -> Option<&str> {
        match self {
            Self::Catalog => None,
            Self::Family(value)
            | Self::Profile(value)
            | Self::RuleId(value)
            | Self::Primitive(value) => Some(*value),
        }
    }
}

/// Evaluates one read-only catalog query without constructing a solver service.
pub fn query(request: RuleSpecRequest) -> Result<Value, RuleSpecError> {
    let registry = builtin_registry();
    let selector = request.selector()?;
    let mut response = Map::from_iter([
        (
            "registry_schema_version".to_owned(),
            json!(registry.schema_version()),
        ),
        (
            "query".to_owned(),
            json!({
                "selector": selector.name(),
                "value": selector.value(),
                "include": request.include,
            }),
        ),
        ("capability".to_owned(), json!({ "status": "implemented" })),
        (
            "next_tools".to_owned(),
            json!(["solve_rule_spec", "solve_rules"]),
        ),
    ]);

    match selector {
        RuleSpecSelector::Catalog => {
            response.insert("families".to_owned(), json!(registry.families()));
        }
        RuleSpecSelector::Family(id) => {
            let family = registry
                .family(id)
                .ok_or_else(|| RuleSpecError::UnknownSelector {
                    selector: "family",
                    value: id.to_owned(),
                })?;
            let profiles = registry
                .profiles()
                .iter()
                .filter(|profile| profile.family() == id)
                .collect::<Vec<_>>();
            response.insert("family".to_owned(), json!(family));
            response.insert("profiles".to_owned(), json!(profiles));
        }
        RuleSpecSelector::Profile(id) => {
            let profile = registry
                .profile(id)
                .ok_or_else(|| RuleSpecError::UnknownSelector {
                    selector: "profile",
                    value: id.to_owned(),
                })?;
            let rules = registry
                .rules()
                .iter()
                .filter(|rule| rule.profile() == id)
                .map(|rule| project_rule(rule, request.include))
                .collect::<Result<Vec<_>, _>>()?;
            response.insert("profile".to_owned(), json!(profile));
            response.insert("rules".to_owned(), Value::Array(rules));
        }
        RuleSpecSelector::RuleId(id) => {
            let rule = registry
                .rule(id)
                .ok_or_else(|| RuleSpecError::UnknownSelector {
                    selector: "rule_id",
                    value: id.to_owned(),
                })?;
            response.insert("rule".to_owned(), project_rule(rule, request.include)?);
        }
        RuleSpecSelector::Primitive(primitive) => {
            let rules = registry
                .rules()
                .iter()
                .filter(|rule| rule.primitive() == primitive)
                .map(|rule| project_rule(rule, request.include))
                .collect::<Result<Vec<_>, _>>()?;
            if rules.is_empty() {
                return Err(RuleSpecError::UnknownSelector {
                    selector: "primitive",
                    value: primitive.to_owned(),
                });
            }
            response.insert("rules".to_owned(), Value::Array(rules));
        }
    }

    Ok(Value::Object(response))
}

fn project_rule(rule: &RuleDefinition, include: RuleSpecInclude) -> Result<Value, RuleSpecError> {
    let Value::Object(mut source) =
        serde_json::to_value(rule).map_err(|error| RuleSpecError::CatalogSerialization {
            message: error.to_string(),
        })?
    else {
        return Err(RuleSpecError::CatalogSerialization {
            message: "serialized rule was not an object".to_owned(),
        });
    };

    let examples = source.remove("examples");
    let llm_encoding = source.remove("llm_encoding");
    let solver_encoding = source.remove("solver_encoding");

    match include {
        RuleSpecInclude::Summary => {}
        RuleSpecInclude::ValidExample => {
            insert_example(&mut source, examples.as_ref(), "valid", "valid_example");
        }
        RuleSpecInclude::InvalidExample => {
            insert_example(&mut source, examples.as_ref(), "invalid", "invalid_example");
        }
        RuleSpecInclude::LlmEncoding => {
            insert_if_some(&mut source, "llm_encoding", llm_encoding);
        }
        RuleSpecInclude::SolverEncoding => {
            insert_if_some(&mut source, "solver_encoding", solver_encoding);
        }
        RuleSpecInclude::All => {
            insert_example(&mut source, examples.as_ref(), "valid", "valid_example");
            insert_example(&mut source, examples.as_ref(), "invalid", "invalid_example");
            insert_if_some(&mut source, "llm_encoding", llm_encoding);
            insert_if_some(&mut source, "solver_encoding", solver_encoding);
        }
    }

    Ok(Value::Object(source))
}

fn insert_example(
    target: &mut Map<String, Value>,
    examples: Option<&Value>,
    source_key: &str,
    target_key: &str,
) {
    if let Some(example) = examples.and_then(|value| value.get(source_key)).cloned() {
        target.insert(target_key.to_owned(), example);
    }
}

fn insert_if_some(target: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    if let Some(value) = value {
        target.insert(key.to_owned(), value);
    }
}

/// Stable request and catalog errors returned by `solve_rule_spec`.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum RuleSpecError {
    /// More than one selector was provided.
    #[error("at most one selector may be provided: family, profile, rule_id, or primitive")]
    AmbiguousSelector,
    /// An exact selector or primitive did not exist.
    #[error("unknown {selector} `{value}`")]
    UnknownSelector {
        selector: &'static str,
        value: String,
    },
    /// Static catalog metadata could not be serialized.
    #[error("could not serialize rule catalog: {message}")]
    CatalogSerialization { message: String },
}
