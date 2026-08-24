//! Finite configuration validation and typed constraint lowering.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    rules::{
        compiler::{FamilyCompilation, FamilyCompileError, ModelProjection, RuleFamilyCompiler},
        manifest::validate_binding_contract,
        manifest_family_executable_rule_ids,
        manifest_format::NativeHandlerV1,
        primitives::{add, and, boolean, eq, ge, int, le, mul, not, or, request, var},
        CompiledRule, RuleSolveMode,
    },
    types::{
        ConstraintExpr, Variable, DEFAULT_TIMEOUT_MS, MAX_CONSTRAINTS, MAX_TIMEOUT_MS,
        MAX_VARIABLES,
    },
};

const MAX_CONFIGURATION_EXPRESSION_NODES: usize = MAX_CONSTRAINTS * MAX_VARIABLES;

/// Configuration compiler registered behind `solve_rules`.
pub static COMPILER: ConfigurationCompiler = ConfigurationCompiler;

/// Stateless finite-configuration compiler.
pub struct ConfigurationCompiler;

impl RuleFamilyCompiler for ConfigurationCompiler {
    fn id(&self) -> &'static str {
        "configuration"
    }

    fn input_schema(&self) -> Value {
        input_schema()
    }

    fn compile(&self, input: Value) -> Result<FamilyCompilation, FamilyCompileError> {
        compile(input).map_err(|message| FamilyCompileError::new(self.id(), message))
    }
}

fn compile(input: Value) -> Result<FamilyCompilation, String> {
    let input: ConfigurationRequest =
        serde_json::from_value(input).map_err(|error| error.to_string())?;
    if input.rules.is_empty() {
        return Err("at least one configuration rule binding is required".to_owned());
    }
    if input.rules.len() > MAX_CONSTRAINTS {
        return Err(format!(
            "request has too many configuration rule bindings; maximum is {MAX_CONSTRAINTS}"
        ));
    }
    if input.mode == RuleSolveMode::Verify && !input.unknowns.is_empty() {
        return Err("verification requires complete configuration facts".to_owned());
    }

    let bindings = input
        .rules
        .iter()
        .map(validate_manifest_binding)
        .collect::<Result<Vec<_>, _>>()?;
    validate_facts(&input.facts, &input.unknowns)?;
    validate_expression_budget(&bindings, &input.facts)?;
    let mut resolver = ConfigurationResolver::new(input.facts, &input.unknowns)?;
    let mut rules = Vec::with_capacity(bindings.len());
    for (index, binding) in bindings.iter().enumerate() {
        rules.push(CompiledRule::new(
            binding.source.rule_id.clone(),
            index,
            compile_binding(binding, &mut resolver)?,
        ));
    }
    let solver_request = request(
        "configuration",
        resolver.variables,
        &rules,
        input.timeout_ms,
        input.persist,
        input.include_smt,
    );
    solver_request
        .validate()
        .map_err(|error| format!("compiled configuration rules are invalid: {error}"))?;

    Ok(FamilyCompilation {
        mode: input.mode,
        request: solver_request,
        rules,
        projections: resolver.projections,
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigurationRequest {
    #[serde(rename = "family")]
    _family: String,
    mode: RuleSolveMode,
    rules: Vec<ConfigurationRuleBinding>,
    facts: ConfigurationFacts,
    #[serde(default)]
    unknowns: Vec<ConfigurationUnknown>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    persist: bool,
    #[serde(default)]
    include_smt: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigurationRuleBinding {
    rule_id: String,
    subjects: Vec<String>,
    #[serde(default)]
    parameters: ConfigurationParameters,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ConfigurationParameters {}

struct ValidatedConfigurationBinding<'a> {
    source: &'a ConfigurationRuleBinding,
    handler: NativeHandlerV1,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigurationFacts {
    components: BTreeMap<String, ComponentFacts>,
    selection_groups: BTreeMap<String, SelectionGroupFacts>,
    allowed_attribute_pairs: Vec<AllowedAttributePairFacts>,
    version_orderings: BTreeMap<String, VersionOrderingFacts>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ComponentFacts {
    selected: Option<bool>,
    attributes: BTreeMap<String, Option<String>>,
    #[serde(default)]
    version: Option<ComponentVersionFacts>,
    #[serde(default)]
    version_requirements: BTreeMap<String, VersionRequirementFacts>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ComponentVersionFacts {
    ordering: String,
    rank: Option<i64>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionRequirementFacts {
    ordering: String,
    minimum_rank: i64,
    maximum_rank: i64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectionGroupFacts {
    active: Option<bool>,
    members: Vec<String>,
    minimum: i64,
    maximum: i64,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowedAttributePairFacts {
    left: AttributeEndpointFacts,
    right: AttributeEndpointFacts,
    allowed: Vec<[String; 2]>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct AttributeEndpointFacts {
    component: String,
    attribute: String,
    values: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionOrderingFacts {
    minimum_rank: i64,
    maximum_rank: i64,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ConfigurationUnknown {
    ComponentSelected {
        component: String,
    },
    SelectionGroupActive {
        group: String,
    },
    ComponentAttribute {
        component: String,
        attribute: String,
        values: Vec<String>,
    },
    ComponentVersionRank {
        component: String,
    },
}

impl ConfigurationUnknown {
    fn stable_key(&self) -> String {
        match self {
            Self::ComponentSelected { component } => format!("component:{component}:selected"),
            Self::SelectionGroupActive { group } => format!("group:{group}:active"),
            Self::ComponentAttribute {
                component,
                attribute,
                ..
            } => format!("component:{component}:attribute:{attribute}"),
            Self::ComponentVersionRank { component } => {
                format!("component:{component}:version_rank")
            }
        }
    }
}

fn validate_manifest_binding(
    binding: &ConfigurationRuleBinding,
) -> Result<ValidatedConfigurationBinding<'_>, String> {
    let parameters =
        serde_json::to_value(&binding.parameters).map_err(|error| error.to_string())?;
    let Value::Object(parameters) = parameters else {
        return Err("configuration parameters did not serialize as an object".to_owned());
    };
    let validated = validate_binding_contract(&binding.rule_id, &binding.subjects, &parameters)?;
    Ok(ValidatedConfigurationBinding {
        source: binding,
        handler: validated.handler,
    })
}

fn validate_facts(
    facts: &ConfigurationFacts,
    unknowns: &[ConfigurationUnknown],
) -> Result<(), String> {
    if facts.components.len() > MAX_CONSTRAINTS
        || facts.selection_groups.len() > MAX_CONSTRAINTS
        || facts.allowed_attribute_pairs.len() > MAX_CONSTRAINTS
        || facts.version_orderings.len() > MAX_CONSTRAINTS
    {
        return Err("configuration facts exceed family limits".to_owned());
    }
    if unknowns.len() > MAX_VARIABLES {
        return Err(format!("configuration unknown maximum is {MAX_VARIABLES}"));
    }

    for (ordering, bounds) in &facts.version_orderings {
        if bounds.minimum_rank < 0 || bounds.minimum_rank > bounds.maximum_rank {
            return Err(format!(
                "version ordering `{ordering}` must have 0 <= minimum_rank <= maximum_rank"
            ));
        }
    }

    for (component, component_facts) in &facts.components {
        if component.is_empty() {
            return Err("configuration component IDs must not be empty".to_owned());
        }
        if component_facts.attributes.len() > MAX_CONSTRAINTS
            || component_facts.version_requirements.len() > MAX_CONSTRAINTS
        {
            return Err(format!("component `{component}` exceeds family limits"));
        }
        if let Some(version) = &component_facts.version {
            let ordering = require_ordering(facts, &version.ordering)?;
            if version
                .rank
                .is_some_and(|rank| rank < ordering.minimum_rank || rank > ordering.maximum_rank)
            {
                return Err(format!(
                    "components.{component}.version.rank is outside ordering `{}` bounds",
                    version.ordering
                ));
            }
        }
        for (provider, requirement) in &component_facts.version_requirements {
            let provider_facts = facts.components.get(provider).ok_or_else(|| {
                format!("component `{component}` requires unknown component `{provider}`")
            })?;
            let ordering = require_ordering(facts, &requirement.ordering)?;
            if requirement.minimum_rank < ordering.minimum_rank
                || requirement.minimum_rank > requirement.maximum_rank
                || requirement.maximum_rank > ordering.maximum_rank
            {
                return Err(format!(
                    "component `{component}` version requirement for `{provider}` must fit ordering `{}` bounds",
                    requirement.ordering
                ));
            }
            let provider_version = provider_facts
                .version
                .as_ref()
                .ok_or_else(|| format!("component `{provider}` has no ranked version facts"))?;
            if provider_version.ordering != requirement.ordering {
                return Err(format!(
                    "component `{component}` and provider `{provider}` use different version orderings"
                ));
            }
        }
    }

    for (group, group_facts) in &facts.selection_groups {
        if group_facts.members.is_empty() {
            return Err(format!("selection group `{group}` must have members"));
        }
        reject_duplicates(
            &group_facts.members,
            &format!("selection group `{group}` members"),
        )?;
        for member in &group_facts.members {
            require_component(facts, member)?;
        }
        let member_count = i64::try_from(group_facts.members.len())
            .map_err(|_| format!("selection group `{group}` member count is too large"))?;
        if group_facts.minimum < 0
            || group_facts.minimum > group_facts.maximum
            || group_facts.maximum > member_count
        {
            return Err(format!(
                "selection group `{group}` must have 0 <= minimum <= maximum <= member count"
            ));
        }
    }

    let mut unknown_keys = BTreeSet::new();
    let mut attribute_domains = BTreeMap::new();
    for unknown in unknowns {
        let key = unknown.stable_key();
        match unknown {
            ConfigurationUnknown::ComponentSelected { component } => {
                let component_facts = require_component(facts, component)?;
                if component_facts.selected.is_some() {
                    return Err(format!("components.{component}.selected is already fixed"));
                }
            }
            ConfigurationUnknown::SelectionGroupActive { group } => {
                let group_facts = facts
                    .selection_groups
                    .get(group)
                    .ok_or_else(|| format!("unknown configuration selection group `{group}`"))?;
                if group_facts.active.is_some() {
                    return Err(format!("selection_groups.{group}.active is already fixed"));
                }
            }
            ConfigurationUnknown::ComponentAttribute {
                component,
                attribute,
                values,
            } => {
                validate_enum_values(
                    values,
                    &format!("components.{component}.attributes.{attribute}"),
                )?;
                let component_facts = require_component(facts, component)?;
                let value = component_facts.attributes.get(attribute).ok_or_else(|| {
                    format!("components.{component}.attributes.{attribute} is not declared")
                })?;
                if value.is_some() {
                    return Err(format!(
                        "components.{component}.attributes.{attribute} is already fixed"
                    ));
                }
                attribute_domains.insert(
                    (component.clone(), attribute.clone()),
                    values.iter().cloned().collect::<BTreeSet<_>>(),
                );
            }
            ConfigurationUnknown::ComponentVersionRank { component } => {
                let component_facts = require_component(facts, component)?;
                let version = component_facts.version.as_ref().ok_or_else(|| {
                    format!("component `{component}` has no ranked version facts")
                })?;
                require_ordering(facts, &version.ordering)?;
                if version.rank.is_some() {
                    return Err(format!(
                        "components.{component}.version.rank is already fixed"
                    ));
                }
            }
        }
        if !unknown_keys.insert(key.clone()) {
            return Err(format!("duplicate configuration unknown `{key}`"));
        }
    }

    for (component, component_facts) in &facts.components {
        require_declared_unknown(
            component_facts.selected.is_none(),
            &unknown_keys,
            &format!("component:{component}:selected"),
            &format!("components.{component}.selected"),
        )?;
        for (attribute, value) in &component_facts.attributes {
            require_declared_unknown(
                value.is_none(),
                &unknown_keys,
                &format!("component:{component}:attribute:{attribute}"),
                &format!("components.{component}.attributes.{attribute}"),
            )?;
        }
        if let Some(version) = &component_facts.version {
            require_declared_unknown(
                version.rank.is_none(),
                &unknown_keys,
                &format!("component:{component}:version_rank"),
                &format!("components.{component}.version.rank"),
            )?;
        }
    }
    for (group, group_facts) in &facts.selection_groups {
        require_declared_unknown(
            group_facts.active.is_none(),
            &unknown_keys,
            &format!("group:{group}:active"),
            &format!("selection_groups.{group}.active"),
        )?;
    }

    let mut component_pairs = BTreeSet::new();
    for pair in &facts.allowed_attribute_pairs {
        let pair_key = (pair.left.component.clone(), pair.right.component.clone());
        if !component_pairs.insert(pair_key.clone()) {
            return Err(format!(
                "duplicate allowed attribute relation for `{}` and `{}`",
                pair_key.0, pair_key.1
            ));
        }
        validate_endpoint(facts, &attribute_domains, &pair.left)?;
        validate_endpoint(facts, &attribute_domains, &pair.right)?;
        let left_values = pair.left.values.iter().collect::<BTreeSet<_>>();
        let right_values = pair.right.values.iter().collect::<BTreeSet<_>>();
        let mut tuples = BTreeSet::new();
        for [left, right] in &pair.allowed {
            if !left_values.contains(left) || !right_values.contains(right) {
                return Err(format!(
                    "allowed attribute tuple `{left}/{right}` is outside its declared domains"
                ));
            }
            if !tuples.insert((left, right)) {
                return Err(format!(
                    "allowed attribute relation for `{}` and `{}` contains duplicate tuples",
                    pair.left.component, pair.right.component
                ));
            }
        }
    }
    Ok(())
}

fn require_component<'a>(
    facts: &'a ConfigurationFacts,
    component: &str,
) -> Result<&'a ComponentFacts, String> {
    facts
        .components
        .get(component)
        .ok_or_else(|| format!("unknown configuration component `{component}`"))
}

fn require_ordering<'a>(
    facts: &'a ConfigurationFacts,
    ordering: &str,
) -> Result<&'a VersionOrderingFacts, String> {
    facts
        .version_orderings
        .get(ordering)
        .ok_or_else(|| format!("unknown configuration version ordering `{ordering}`"))
}

fn require_declared_unknown(
    missing: bool,
    unknown_keys: &BTreeSet<String>,
    key: &str,
    path: &str,
) -> Result<(), String> {
    if missing && !unknown_keys.contains(key) {
        return Err(format!("{path} is null and has no unknown declaration"));
    }
    Ok(())
}

fn validate_enum_values(values: &[String], path: &str) -> Result<(), String> {
    if values.is_empty() || values.len() > MAX_CONSTRAINTS {
        return Err(format!("{path} must declare non-empty finite values"));
    }
    if values.iter().any(String::is_empty) {
        return Err(format!("{path} enum labels must not be empty"));
    }
    reject_duplicates(values, &format!("{path} values"))
}

fn validate_endpoint(
    facts: &ConfigurationFacts,
    attribute_domains: &BTreeMap<(String, String), BTreeSet<String>>,
    endpoint: &AttributeEndpointFacts,
) -> Result<(), String> {
    validate_enum_values(
        &endpoint.values,
        &format!(
            "components.{}.attributes.{}",
            endpoint.component, endpoint.attribute
        ),
    )?;
    let component = require_component(facts, &endpoint.component)?;
    let value = component
        .attributes
        .get(&endpoint.attribute)
        .ok_or_else(|| {
            format!(
                "components.{}.attributes.{} is not declared",
                endpoint.component, endpoint.attribute
            )
        })?;
    let values = endpoint.values.iter().cloned().collect::<BTreeSet<_>>();
    match value {
        Some(value) if !values.contains(value) => Err(format!(
            "components.{}.attributes.{} value `{value}` is outside its declared domain",
            endpoint.component, endpoint.attribute
        )),
        None => {
            let unknown_values = attribute_domains
                .get(&(endpoint.component.clone(), endpoint.attribute.clone()))
                .expect("null attributes require a validated unknown");
            if unknown_values != &values {
                return Err(format!(
                    "components.{}.attributes.{} unknown domain does not match the allowed-pair domain",
                    endpoint.component, endpoint.attribute
                ));
            }
            Ok(())
        }
        Some(_) => Ok(()),
    }
}

fn reject_duplicates(items: &[String], context: &str) -> Result<(), String> {
    if items.iter().collect::<BTreeSet<_>>().len() != items.len() {
        return Err(format!("{context} contains duplicates"));
    }
    Ok(())
}

fn validate_expression_budget(
    bindings: &[ValidatedConfigurationBinding<'_>],
    facts: &ConfigurationFacts,
) -> Result<(), String> {
    let mut total = 0usize;
    for binding in bindings {
        total = total
            .checked_add(binding_expression_node_count(binding, facts)?)
            .ok_or_else(expression_size_overflow)?;
        if total > MAX_CONFIGURATION_EXPRESSION_NODES {
            return Err(format!(
                "configuration expressions exceed the checked node budget {MAX_CONFIGURATION_EXPRESSION_NODES}"
            ));
        }
    }
    Ok(())
}

fn binding_expression_node_count(
    binding: &ValidatedConfigurationBinding<'_>,
    facts: &ConfigurationFacts,
) -> Result<usize, String> {
    let subjects = &binding.source.subjects;
    match binding.handler {
        NativeHandlerV1::ConfigurationRequiresAny => subjects
            .len()
            .checked_add(2)
            .ok_or_else(expression_size_overflow),
        NativeHandlerV1::ConfigurationExcludes => Ok(5),
        NativeHandlerV1::ConfigurationSelectionCardinality => {
            let group_id = &subjects[0];
            let group = facts
                .selection_groups
                .get(group_id)
                .ok_or_else(|| format!("unknown configuration selection group `{group_id}`"))?;
            let active_link_nodes = indicator_link_node_count(group.active);
            let mut member_link_nodes = 0usize;
            for member in &group.members {
                member_link_nodes = member_link_nodes
                    .checked_add(indicator_link_node_count(
                        require_component(facts, member)?.selected,
                    ))
                    .ok_or_else(expression_size_overflow)?;
            }
            let member_comparison_nodes = group
                .members
                .len()
                .checked_mul(3)
                .ok_or_else(expression_size_overflow)?;
            let selected_nodes =
                grouped_expression_node_count(group.members.len(), group.members.len())?;
            let bound_nodes = selected_nodes
                .checked_mul(2)
                .and_then(|nodes| nodes.checked_add(8))
                .ok_or_else(expression_size_overflow)?;
            active_link_nodes
                .checked_add(member_link_nodes)
                .and_then(|nodes| nodes.checked_add(member_comparison_nodes))
                .and_then(|nodes| nodes.checked_add(bound_nodes))
                .and_then(|nodes| nodes.checked_add(1))
                .ok_or_else(expression_size_overflow)
        }
        NativeHandlerV1::ConfigurationAttributeAllowedPair => {
            let matches = facts
                .allowed_attribute_pairs
                .iter()
                .filter(|pair| {
                    pair.left.component == subjects[0] && pair.right.component == subjects[1]
                })
                .collect::<Vec<_>>();
            let [pair] = matches.as_slice() else {
                return Err(format!(
                    "configuration.attribute_allowed_pair requires exactly one relation for `{}` and `{}`",
                    subjects[0], subjects[1]
                ));
            };
            let left_nodes = attribute_match_node_count(facts, &pair.left)?;
            let right_nodes = attribute_match_node_count(facts, &pair.right)?;
            let tuple_nodes = 1usize
                .checked_add(left_nodes)
                .and_then(|nodes| nodes.checked_add(right_nodes))
                .ok_or_else(expression_size_overflow)?;
            let allowed_children = pair
                .allowed
                .len()
                .checked_mul(tuple_nodes)
                .ok_or_else(expression_size_overflow)?;
            grouped_expression_node_count(allowed_children, pair.allowed.len())?
                .checked_add(5)
                .ok_or_else(expression_size_overflow)
        }
        NativeHandlerV1::ConfigurationVersionInterval => Ok(11),
        _ => Err(format!(
            "unsupported configuration rule `{}`",
            binding.source.rule_id
        )),
    }
}

fn indicator_link_node_count(fixed: Option<bool>) -> usize {
    // `or(and(source, eq(value, 1)), and(not(source), eq(value, 0)))`.
    usize::from(fixed.is_none()) * 12
}

fn attribute_match_node_count(
    facts: &ConfigurationFacts,
    endpoint: &AttributeEndpointFacts,
) -> Result<usize, String> {
    let value = require_component(facts, &endpoint.component)?
        .attributes
        .get(&endpoint.attribute)
        .ok_or_else(|| {
            format!(
                "components.{}.attributes.{} is not declared",
                endpoint.component, endpoint.attribute
            )
        })?;
    Ok(if value.is_some() { 1 } else { 3 })
}

fn grouped_expression_node_count(children: usize, child_count: usize) -> Result<usize, String> {
    match child_count {
        0 => Ok(1),
        1 => Ok(children),
        _ => children.checked_add(1).ok_or_else(expression_size_overflow),
    }
}

fn expression_size_overflow() -> String {
    "configuration expression size overflowed".to_owned()
}

fn compile_binding(
    binding: &ValidatedConfigurationBinding<'_>,
    resolver: &mut ConfigurationResolver,
) -> Result<ConstraintExpr, String> {
    let subjects = &binding.source.subjects;
    match binding.handler {
        NativeHandlerV1::ConfigurationRequiresAny => {
            reject_duplicates(subjects, "configuration.requires_any subjects")?;
            let mut predicate = vec![not(resolver.component_selected(&subjects[0])?)];
            for provider in &subjects[1..] {
                predicate.push(resolver.component_selected(provider)?);
            }
            Ok(or(predicate))
        }
        NativeHandlerV1::ConfigurationExcludes => {
            reject_duplicates(subjects, "configuration.excludes subjects")?;
            Ok(or(vec![
                not(resolver.component_selected(&subjects[0])?),
                not(resolver.component_selected(&subjects[1])?),
            ]))
        }
        NativeHandlerV1::ConfigurationSelectionCardinality => {
            let group_id = &subjects[0];
            let group = resolver
                .facts
                .selection_groups
                .get(group_id)
                .cloned()
                .ok_or_else(|| format!("unknown configuration selection group `{group_id}`"))?;
            let active = resolver.group_indicator(group_id)?;
            let mut members = Vec::with_capacity(group.members.len());
            for member in &group.members {
                members.push(resolver.component_indicator(member)?);
            }
            let mut predicates = Vec::new();
            if let Some(link) = active.link.clone() {
                predicates.push(link);
            }
            for member in &members {
                if let Some(link) = member.link.clone() {
                    predicates.push(link);
                }
                predicates.push(le(member.value.clone(), active.value.clone()));
            }
            let selected = sum(members.into_iter().map(|item| item.value).collect());
            predicates.push(le(
                mul(vec![int(group.minimum), active.value.clone()]),
                selected.clone(),
            ));
            predicates.push(le(selected, mul(vec![int(group.maximum), active.value])));
            Ok(conjunction(predicates))
        }
        NativeHandlerV1::ConfigurationAttributeAllowedPair => {
            let matches = resolver
                .facts
                .allowed_attribute_pairs
                .iter()
                .filter(|pair| {
                    pair.left.component == subjects[0] && pair.right.component == subjects[1]
                })
                .cloned()
                .collect::<Vec<_>>();
            let [pair] = matches.as_slice() else {
                return Err(format!(
                    "configuration.attribute_allowed_pair requires exactly one relation for `{}` and `{}`",
                    subjects[0], subjects[1]
                ));
            };
            let active = and(vec![
                resolver.component_selected(&subjects[0])?,
                resolver.component_selected(&subjects[1])?,
            ]);
            let allowed = pair
                .allowed
                .iter()
                .map(|[left, right]| {
                    Ok(and(vec![
                        resolver.attribute_matches(
                            &pair.left.component,
                            &pair.left.attribute,
                            left,
                        )?,
                        resolver.attribute_matches(
                            &pair.right.component,
                            &pair.right.attribute,
                            right,
                        )?,
                    ]))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(or(vec![not(active), disjunction(allowed)]))
        }
        NativeHandlerV1::ConfigurationVersionInterval => {
            reject_duplicates(subjects, "configuration.version_interval subjects")?;
            let consumer = &subjects[0];
            let provider = &subjects[1];
            let requirement = resolver
                .facts
                .components
                .get(consumer)
                .ok_or_else(|| format!("unknown configuration component `{consumer}`"))?
                .version_requirements
                .get(provider)
                .cloned()
                .ok_or_else(|| {
                    format!("component `{consumer}` has no version requirement for `{provider}`")
                })?;
            let provider_version = resolver.facts.components[provider]
                .version
                .as_ref()
                .expect("version requirements validate provider facts");
            if provider_version.ordering != requirement.ordering {
                return Err(format!(
                    "component `{consumer}` and provider `{provider}` use different version orderings"
                ));
            }
            let rank = resolver.version_rank(provider)?;
            Ok(or(vec![
                not(resolver.component_selected(consumer)?),
                and(vec![
                    resolver.component_selected(provider)?,
                    ge(rank.clone(), int(requirement.minimum_rank)),
                    le(rank, int(requirement.maximum_rank)),
                ]),
            ]))
        }
        _ => Err(format!(
            "unsupported configuration rule `{}`",
            binding.source.rule_id
        )),
    }
}

#[derive(Clone)]
struct Indicator {
    value: ConstraintExpr,
    link: Option<ConstraintExpr>,
}

struct ConfigurationResolver {
    facts: ConfigurationFacts,
    component_selected: BTreeMap<String, String>,
    group_active: BTreeMap<String, String>,
    attributes: BTreeMap<(String, String), String>,
    version_ranks: BTreeMap<String, String>,
    indicator_aliases: BTreeMap<String, String>,
    variables: Vec<Variable>,
    projections: Vec<ModelProjection>,
}

impl ConfigurationResolver {
    fn new(facts: ConfigurationFacts, unknowns: &[ConfigurationUnknown]) -> Result<Self, String> {
        let mut component_selected = BTreeMap::new();
        let mut group_active = BTreeMap::new();
        let mut attributes = BTreeMap::new();
        let mut version_ranks = BTreeMap::new();
        let mut variables = Vec::with_capacity(unknowns.len());
        let mut projections = Vec::with_capacity(unknowns.len());
        let mut sorted = unknowns.to_vec();
        sorted.sort_by_key(ConfigurationUnknown::stable_key);
        for (index, unknown) in sorted.into_iter().enumerate() {
            let variable = format!("configuration_u_{index}");
            match unknown {
                ConfigurationUnknown::ComponentSelected { component } => {
                    variables.push(Variable::Bool {
                        name: variable.clone(),
                    });
                    component_selected.insert(component.clone(), variable.clone());
                    projections.push(ModelProjection {
                        variable,
                        subject: component.clone(),
                        field: format!("components.{component}.selected"),
                    });
                }
                ConfigurationUnknown::SelectionGroupActive { group } => {
                    variables.push(Variable::Bool {
                        name: variable.clone(),
                    });
                    group_active.insert(group.clone(), variable.clone());
                    projections.push(ModelProjection {
                        variable,
                        subject: group.clone(),
                        field: format!("selection_groups.{group}.active"),
                    });
                }
                ConfigurationUnknown::ComponentAttribute {
                    component,
                    attribute,
                    values,
                } => {
                    variables.push(Variable::Enum {
                        name: variable.clone(),
                        values,
                    });
                    attributes.insert((component.clone(), attribute.clone()), variable.clone());
                    projections.push(ModelProjection {
                        variable,
                        subject: component.clone(),
                        field: format!("components.{component}.attributes.{attribute}"),
                    });
                }
                ConfigurationUnknown::ComponentVersionRank { component } => {
                    let version = facts.components[&component]
                        .version
                        .as_ref()
                        .expect("version unknown validation requires version facts");
                    let bounds = &facts.version_orderings[&version.ordering];
                    variables.push(Variable::IntRange {
                        name: variable.clone(),
                        min: bounds.minimum_rank,
                        max: bounds.maximum_rank,
                    });
                    version_ranks.insert(component.clone(), variable.clone());
                    projections.push(ModelProjection {
                        variable,
                        subject: component.clone(),
                        field: format!("components.{component}.version.rank"),
                    });
                }
            }
        }
        Ok(Self {
            facts,
            component_selected,
            group_active,
            attributes,
            version_ranks,
            indicator_aliases: BTreeMap::new(),
            variables,
            projections,
        })
    }

    fn component_selected(&self, component: &str) -> Result<ConstraintExpr, String> {
        let facts = require_component(&self.facts, component)?;
        Ok(facts
            .selected
            .map_or_else(|| var(self.component_selected[component].clone()), boolean))
    }

    fn group_active(&self, group: &str) -> Result<ConstraintExpr, String> {
        let facts = self
            .facts
            .selection_groups
            .get(group)
            .ok_or_else(|| format!("unknown configuration selection group `{group}`"))?;
        Ok(facts
            .active
            .map_or_else(|| var(self.group_active[group].clone()), boolean))
    }

    fn component_indicator(&mut self, component: &str) -> Result<Indicator, String> {
        let fixed = require_component(&self.facts, component)?.selected;
        match fixed {
            Some(value) => Ok(Indicator {
                value: int(i64::from(value)),
                link: None,
            }),
            None => {
                let selected = self.component_selected(component)?;
                self.boolean_indicator(format!("component:{component}:selected"), selected)
            }
        }
    }

    fn group_indicator(&mut self, group: &str) -> Result<Indicator, String> {
        let fixed = self
            .facts
            .selection_groups
            .get(group)
            .ok_or_else(|| format!("unknown configuration selection group `{group}`"))?
            .active;
        match fixed {
            Some(value) => Ok(Indicator {
                value: int(i64::from(value)),
                link: None,
            }),
            None => {
                let active = self.group_active(group)?;
                self.boolean_indicator(format!("group:{group}:active"), active)
            }
        }
    }

    fn boolean_indicator(
        &mut self,
        key: String,
        source: ConstraintExpr,
    ) -> Result<Indicator, String> {
        let alias = if let Some(alias) = self.indicator_aliases.get(&key) {
            alias.clone()
        } else {
            if self.variables.len() >= MAX_VARIABLES {
                return Err(format!(
                    "configuration variables exceed maximum {MAX_VARIABLES}"
                ));
            }
            let alias = format!("configuration_i_{}", self.indicator_aliases.len());
            self.variables.push(Variable::IntRange {
                name: alias.clone(),
                min: 0,
                max: 1,
            });
            self.indicator_aliases.insert(key, alias.clone());
            alias
        };
        let value = var(alias.clone());
        let link = or(vec![
            and(vec![source.clone(), eq(value.clone(), int(1))]),
            and(vec![not(source), eq(value.clone(), int(0))]),
        ]);
        Ok(Indicator {
            value,
            link: Some(link),
        })
    }

    fn attribute_matches(
        &self,
        component: &str,
        attribute: &str,
        label: &str,
    ) -> Result<ConstraintExpr, String> {
        let value = require_component(&self.facts, component)?
            .attributes
            .get(attribute)
            .ok_or_else(|| {
                format!("components.{component}.attributes.{attribute} is not declared")
            })?;
        Ok(match value {
            Some(value) => boolean(value == label),
            None => {
                let variable =
                    self.attributes[&(component.to_owned(), attribute.to_owned())].clone();
                eq(
                    var(variable.clone()),
                    ConstraintExpr::EnumLabel {
                        var: variable,
                        label: label.to_owned(),
                    },
                )
            }
        })
    }

    fn version_rank(&self, component: &str) -> Result<ConstraintExpr, String> {
        let version = require_component(&self.facts, component)?
            .version
            .as_ref()
            .ok_or_else(|| format!("component `{component}` has no ranked version facts"))?;
        Ok(version
            .rank
            .map_or_else(|| var(self.version_ranks[component].clone()), int))
    }
}

fn conjunction(predicates: Vec<ConstraintExpr>) -> ConstraintExpr {
    match predicates.len() {
        0 => boolean(true),
        1 => predicates.into_iter().next().expect("one predicate"),
        _ => and(predicates),
    }
}

fn disjunction(predicates: Vec<ConstraintExpr>) -> ConstraintExpr {
    match predicates.len() {
        0 => boolean(false),
        1 => predicates.into_iter().next().expect("one predicate"),
        _ => or(predicates),
    }
}

fn sum(expressions: Vec<ConstraintExpr>) -> ConstraintExpr {
    match expressions.len() {
        0 => int(0),
        1 => expressions.into_iter().next().expect("one expression"),
        _ => add(expressions),
    }
}

const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn input_schema() -> Value {
    let rule_ids = manifest_family_executable_rule_ids("configuration")
        .expect("configuration manifest executable rule IDs");
    let version_requirement = json!({
        "type": "object",
        "properties": {
            "ordering": {"type": "string"},
            "minimum_rank": {"type": "integer", "minimum": 0},
            "maximum_rank": {"type": "integer", "minimum": 0}
        },
        "required": ["ordering", "minimum_rank", "maximum_rank"],
        "additionalProperties": false
    });
    let endpoint = json!({
        "type": "object",
        "properties": {
            "component": {"type": "string"},
            "attribute": {"type": "string"},
            "values": {"type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS, "items": {"type": "string"}}
        },
        "required": ["component", "attribute", "values"],
        "additionalProperties": false
    });
    json!({
        "type": "object",
        "properties": {
            "family": {"const": "configuration"},
            "mode": {"type": "string", "enum": ["verify", "synthesize"]},
            "rules": {
                "type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS,
                "items": {
                    "type": "object",
                    "properties": {
                        "rule_id": {"type": "string", "enum": rule_ids},
                        "subjects": {"type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS, "items": {"type": "string"}},
                        "parameters": {"type": "object", "additionalProperties": false}
                    },
                    "required": ["rule_id", "subjects"],
                    "additionalProperties": false
                }
            },
            "facts": {
                "type": "object",
                "properties": {
                    "components": {
                        "type": "object", "maxProperties": MAX_CONSTRAINTS,
                        "additionalProperties": {
                            "type": "object",
                            "properties": {
                                "selected": {"type": ["boolean", "null"]},
                                "attributes": {"type": "object", "maxProperties": MAX_CONSTRAINTS, "additionalProperties": {"type": ["string", "null"]}},
                                "version": {
                                    "type": ["object", "null"],
                                    "properties": {
                                        "ordering": {"type": "string"},
                                        "rank": {"type": ["integer", "null"], "minimum": 0}
                                    },
                                    "required": ["ordering", "rank"],
                                    "additionalProperties": false
                                },
                                "version_requirements": {"type": "object", "maxProperties": MAX_CONSTRAINTS, "additionalProperties": version_requirement}
                            },
                            "required": ["selected", "attributes"],
                            "additionalProperties": false
                        }
                    },
                    "selection_groups": {
                        "type": "object", "maxProperties": MAX_CONSTRAINTS,
                        "additionalProperties": {
                            "type": "object",
                            "properties": {
                                "active": {"type": ["boolean", "null"]},
                                "members": {"type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS, "items": {"type": "string"}},
                                "minimum": {"type": "integer", "minimum": 0},
                                "maximum": {"type": "integer", "minimum": 0}
                            },
                            "required": ["active", "members", "minimum", "maximum"],
                            "additionalProperties": false
                        }
                    },
                    "allowed_attribute_pairs": {
                        "type": "array", "maxItems": MAX_CONSTRAINTS,
                        "items": {
                            "type": "object",
                            "properties": {
                                "left": endpoint,
                                "right": endpoint,
                                "allowed": {
                                    "type": "array", "maxItems": MAX_CONSTRAINTS,
                                    "items": {"type": "array", "minItems": 2, "maxItems": 2, "items": {"type": "string"}}
                                }
                            },
                            "required": ["left", "right", "allowed"],
                            "additionalProperties": false
                        }
                    },
                    "version_orderings": {
                        "type": "object", "maxProperties": MAX_CONSTRAINTS,
                        "additionalProperties": {
                            "type": "object",
                            "properties": {
                                "minimum_rank": {"type": "integer", "minimum": 0},
                                "maximum_rank": {"type": "integer", "minimum": 0}
                            },
                            "required": ["minimum_rank", "maximum_rank"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["components", "selection_groups", "allowed_attribute_pairs", "version_orderings"],
                "additionalProperties": false
            },
            "unknowns": {
                "type": "array", "maxItems": MAX_VARIABLES, "default": [],
                "items": {
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": {"kind": {"const": "component_selected"}, "component": {"type": "string"}},
                            "required": ["kind", "component"], "additionalProperties": false
                        },
                        {
                            "type": "object",
                            "properties": {"kind": {"const": "selection_group_active"}, "group": {"type": "string"}},
                            "required": ["kind", "group"], "additionalProperties": false
                        },
                        {
                            "type": "object",
                            "properties": {
                                "kind": {"const": "component_attribute"}, "component": {"type": "string"},
                                "attribute": {"type": "string"},
                                "values": {"type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS, "items": {"type": "string"}}
                            },
                            "required": ["kind", "component", "attribute", "values"], "additionalProperties": false
                        },
                        {
                            "type": "object",
                            "properties": {"kind": {"const": "component_version_rank"}, "component": {"type": "string"}},
                            "required": ["kind", "component"], "additionalProperties": false
                        }
                    ]
                }
            },
            "timeout_ms": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_MS, "default": DEFAULT_TIMEOUT_MS},
            "persist": {"type": "boolean", "default": false},
            "include_smt": {"type": "boolean", "default": false}
        },
        "required": ["family", "mode", "rules", "facts"],
        "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;
    use crate::{
        rules::compiler::RuleFamilyCompiler,
        service::SolverService,
        types::{ModelValue, SolveStatus, Variable},
    };

    fn empty_facts() -> Value {
        json!({
            "components": {},
            "selection_groups": {},
            "allowed_attribute_pairs": [],
            "version_orderings": {}
        })
    }

    fn request(mode: &str, rule_id: &str, subjects: &[&str], facts: Value) -> Value {
        json!({
            "family": "configuration",
            "mode": mode,
            "rules": [{"rule_id": rule_id, "subjects": subjects}],
            "facts": facts,
            "unknowns": []
        })
    }

    async fn status(input: Value) -> SolveStatus {
        let compiled = compile(input).expect("configuration request must compile");
        SolverService::new()
            .solve_constraints(compiled.request)
            .await
            .expect("configuration request must solve")
            .status
    }

    #[tokio::test]
    async fn requires_any_synthesizes_a_selected_provider() {
        let mut input = request(
            "synthesize",
            "configuration.requires_any",
            &["application", "postgres", "sqlite"],
            json!({
                "components": {
                    "application": {"selected": true, "attributes": {}},
                    "postgres": {"selected": null, "attributes": {}},
                    "sqlite": {"selected": null, "attributes": {}}
                },
                "selection_groups": {},
                "allowed_attribute_pairs": [],
                "version_orderings": {}
            }),
        );
        input["unknowns"] = json!([
            {"kind": "component_selected", "component": "sqlite"},
            {"kind": "component_selected", "component": "postgres"}
        ]);

        let compiled = compile(input).expect("configuration request must compile");
        assert_eq!(compiled.rules.len(), 1);
        assert_eq!(compiled.projections.len(), 2);
        assert_eq!(
            compiled
                .projections
                .iter()
                .map(|projection| projection.field.as_str())
                .collect::<Vec<_>>(),
            vec!["components.postgres.selected", "components.sqlite.selected"]
        );
        assert!(compiled
            .request
            .vars
            .iter()
            .all(|variable| matches!(variable, Variable::Bool { .. })));

        let response = SolverService::new()
            .solve_constraints(compiled.request)
            .await
            .expect("requires-any synthesis must solve");
        assert_eq!(response.status, SolveStatus::Sat);
        let projected = COMPILER.project_model(
            &compiled.projections,
            response.model.as_ref().expect("sat model"),
        );
        assert!(projected
            .iter()
            .any(|assignment| assignment.value == ModelValue::Bool(true)));
    }

    #[tokio::test]
    async fn excludes_rejects_a_selected_pair() {
        let input = request(
            "verify",
            "configuration.excludes",
            &["mysql", "postgres"],
            json!({
                "components": {
                    "mysql": {"selected": true, "attributes": {}},
                    "postgres": {"selected": true, "attributes": {}}
                },
                "selection_groups": {},
                "allowed_attribute_pairs": [],
                "version_orderings": {}
            }),
        );
        assert_eq!(status(input).await, SolveStatus::Unsat);
    }

    #[tokio::test]
    async fn selection_cardinality_accepts_one_member_and_rejects_two() {
        let facts = |second| {
            json!({
                "components": {
                    "postgres": {"selected": true, "attributes": {}},
                    "sqlite": {"selected": second, "attributes": {}}
                },
                "selection_groups": {
                    "database": {
                        "active": true,
                        "members": ["postgres", "sqlite"],
                        "minimum": 1,
                        "maximum": 1
                    }
                },
                "allowed_attribute_pairs": [],
                "version_orderings": {}
            })
        };
        assert_eq!(
            status(request(
                "verify",
                "configuration.selection_cardinality",
                &["database"],
                facts(false),
            ))
            .await,
            SolveStatus::Sat
        );
        assert_eq!(
            status(request(
                "verify",
                "configuration.selection_cardinality",
                &["database"],
                facts(true),
            ))
            .await,
            SolveStatus::Unsat
        );
    }

    #[tokio::test]
    async fn attribute_allowed_pair_rejects_an_undeclared_tuple() {
        let input = request(
            "verify",
            "configuration.attribute_allowed_pair",
            &["application", "database"],
            json!({
                "components": {
                    "application": {"selected": true, "attributes": {"os": "linux"}},
                    "database": {"selected": true, "attributes": {"os": "windows"}}
                },
                "selection_groups": {},
                "allowed_attribute_pairs": [{
                    "left": {"component": "application", "attribute": "os", "values": ["linux", "windows"]},
                    "right": {"component": "database", "attribute": "os", "values": ["linux", "windows"]},
                    "allowed": [["linux", "linux"], ["windows", "windows"]]
                }],
                "version_orderings": {}
            }),
        );
        assert_eq!(status(input).await, SolveStatus::Unsat);
    }

    #[test]
    fn checked_model_size_guard_rejects_repeated_allowed_pair_expansions() {
        let left_values = (0..MAX_CONSTRAINTS)
            .map(|index| format!("left-{index}"))
            .collect::<Vec<_>>();
        let right_values = (0..MAX_CONSTRAINTS)
            .map(|index| format!("right-{index}"))
            .collect::<Vec<_>>();
        let allowed = left_values
            .iter()
            .zip(&right_values)
            .map(|(left, right)| vec![left.clone(), right.clone()])
            .collect::<Vec<_>>();
        let rule = json!({
            "rule_id": "configuration.attribute_allowed_pair",
            "subjects": ["left", "right"]
        });
        let mut input = json!({
            "family": "configuration",
            "mode": "synthesize",
            "rules": [rule.clone()],
            "facts": {
                "components": {
                    "left": {"selected": true, "attributes": {"value": null}},
                    "right": {"selected": true, "attributes": {"value": null}}
                },
                "selection_groups": {},
                "allowed_attribute_pairs": [{
                    "left": {"component": "left", "attribute": "value", "values": left_values},
                    "right": {"component": "right", "attribute": "value", "values": right_values},
                    "allowed": allowed
                }],
                "version_orderings": {}
            },
            "unknowns": [
                {"kind": "component_attribute", "component": "left", "attribute": "value", "values": left_values},
                {"kind": "component_attribute", "component": "right", "attribute": "value", "values": right_values}
            ]
        });

        COMPILER
            .compile(input.clone())
            .expect("one maximum-sized finite relation must remain valid");
        input["rules"] = Value::Array(vec![rule; 10]);
        assert!(COMPILER
            .compile(input)
            .expect_err("repeated allowed-pair AST expansion must be bounded")
            .message
            .contains("checked node budget"));
    }

    #[tokio::test]
    async fn version_interval_synthesizes_only_the_declared_rank() {
        let mut input = request(
            "synthesize",
            "configuration.version_interval",
            &["application", "database"],
            json!({
                "components": {
                    "application": {
                        "selected": true,
                        "attributes": {},
                        "version_requirements": {
                            "database": {"ordering": "database_releases", "minimum_rank": 2, "maximum_rank": 4}
                        }
                    },
                    "database": {
                        "selected": true,
                        "attributes": {},
                        "version": {"ordering": "database_releases", "rank": null}
                    }
                },
                "selection_groups": {},
                "allowed_attribute_pairs": [],
                "version_orderings": {
                    "database_releases": {"minimum_rank": 0, "maximum_rank": 6}
                }
            }),
        );
        input["unknowns"] = json!([
            {"kind": "component_version_rank", "component": "database"}
        ]);

        let compiled = compile(input).expect("version request must compile");
        assert_eq!(compiled.projections.len(), 1);
        assert_eq!(
            compiled.projections[0].field,
            "components.database.version.rank"
        );
        assert!(matches!(
            compiled.request.vars.as_slice(),
            [Variable::IntRange { min: 0, max: 6, .. }]
        ));
        let response = SolverService::new()
            .solve_constraints(compiled.request)
            .await
            .expect("version synthesis must solve");
        assert_eq!(response.status, SolveStatus::Sat);
        let rank = response
            .model
            .as_ref()
            .and_then(|model| model.values().next())
            .expect("rank model value");
        assert!(matches!(rank, ModelValue::Int(2..=4)));
    }

    #[test]
    fn verification_rejects_incomplete_facts() {
        let input = request(
            "verify",
            "configuration.requires_any",
            &["application", "database"],
            json!({
                "components": {
                    "application": {"selected": true, "attributes": {}},
                    "database": {"selected": null, "attributes": {}}
                },
                "selection_groups": {},
                "allowed_attribute_pairs": [],
                "version_orderings": {}
            }),
        );
        assert_eq!(
            compile(input).expect_err("verification facts are incomplete"),
            "components.database.selected is null and has no unknown declaration"
        );
    }

    #[test]
    fn synthesis_rejects_fixed_duplicate_and_unbounded_unknowns() {
        let mut input = request(
            "synthesize",
            "configuration.requires_any",
            &["application", "database"],
            json!({
                "components": {
                    "application": {"selected": true, "attributes": {}},
                    "database": {"selected": false, "attributes": {}}
                },
                "selection_groups": {},
                "allowed_attribute_pairs": [],
                "version_orderings": {}
            }),
        );
        input["unknowns"] = json!([
            {"kind": "component_selected", "component": "database"},
            {"kind": "component_selected", "component": "database"}
        ]);
        assert!(compile(input)
            .expect_err("fixed duplicate unknowns must fail")
            .contains("already fixed"));

        let mut attribute = request(
            "synthesize",
            "configuration.excludes",
            &["application", "database"],
            json!({
                "components": {
                    "application": {"selected": true, "attributes": {"os": null}},
                    "database": {"selected": false, "attributes": {}}
                },
                "selection_groups": {},
                "allowed_attribute_pairs": [],
                "version_orderings": {}
            }),
        );
        attribute["unknowns"] = json!([
            {"kind": "component_attribute", "component": "application", "attribute": "os", "values": []}
        ]);
        assert!(compile(attribute)
            .expect_err("empty enum domain must fail")
            .contains("non-empty finite values"));
    }

    #[test]
    fn fact_validation_rejects_duplicate_references_and_bounds() {
        let duplicate_members = request(
            "verify",
            "configuration.selection_cardinality",
            &["database"],
            json!({
                "components": {"postgres": {"selected": true, "attributes": {}}},
                "selection_groups": {
                    "database": {"active": true, "members": ["postgres", "postgres"], "minimum": 1, "maximum": 1}
                },
                "allowed_attribute_pairs": [],
                "version_orderings": {}
            }),
        );
        assert!(compile(duplicate_members)
            .expect_err("duplicate members must fail")
            .contains("contains duplicates"));

        let missing_reference = request(
            "verify",
            "configuration.requires_any",
            &["application", "missing"],
            json!({
                "components": {"application": {"selected": true, "attributes": {}}},
                "selection_groups": {},
                "allowed_attribute_pairs": [],
                "version_orderings": {}
            }),
        );
        assert!(compile(missing_reference)
            .expect_err("missing subject must fail")
            .contains("unknown configuration component `missing`"));

        let invalid_bounds = request(
            "verify",
            "configuration.selection_cardinality",
            &["database"],
            json!({
                "components": {"postgres": {"selected": true, "attributes": {}}},
                "selection_groups": {
                    "database": {"active": true, "members": ["postgres"], "minimum": 2, "maximum": 1}
                },
                "allowed_attribute_pairs": [],
                "version_orderings": {}
            }),
        );
        assert!(compile(invalid_bounds)
            .expect_err("invalid group bounds must fail")
            .contains("0 <= minimum <= maximum <= member count"));
    }

    #[test]
    fn family_dispatch_lists_only_owned_handlers() {
        let cases: [(&str, &str, &str, &[&str]); 4] = [
            (
                "accessibility",
                include_str!("../accessibility/compile.rs"),
                "\nfn contrast_branch(",
                &[
                    "A11yTargetSize",
                    "A11yFocusNotObscured",
                    "A11yReflow",
                    "A11yTextContrast",
                ],
            ),
            (
                "configuration",
                include_str!("compile.rs"),
                "\n#[derive(Clone)]\nstruct Indicator",
                &[
                    "ConfigurationRequiresAny",
                    "ConfigurationExcludes",
                    "ConfigurationSelectionCardinality",
                    "ConfigurationAttributeAllowedPair",
                    "ConfigurationVersionInterval",
                ],
            ),
            (
                "design",
                include_str!("../design/compile.rs"),
                "\nfn validate_minimum_subject_count(",
                &[
                    "LayoutAxisCapacity",
                    "LayoutContainment",
                    "LayoutNonOverlap",
                    "MediaAspectRatio",
                ],
            ),
            (
                "policy",
                include_str!("../policy/compile.rs"),
                "\nfn require_subjects(",
                &[
                    "RbacPermissionReachable",
                    "RbacRoleHierarchyAcyclic",
                    "RbacStaticSeparationOfDuty",
                    "RbacDynamicSeparationOfDuty",
                    "RbacMinimumPrivilege",
                ],
            ),
        ];

        for (family, source, end_marker, owned_handlers) in cases {
            let (_, after_start) = source
                .split_once("fn compile_binding(")
                .expect("family compiler must define compile_binding");
            let (match_body, _) = after_start
                .split_once(end_marker)
                .expect("compile_binding must retain its expected boundary");

            for handler in owned_handlers {
                assert!(
                    match_body.contains(&format!("NativeHandlerV1::{handler}")),
                    "{family} compiler must explicitly match owned handler {handler}"
                );
            }
            assert_eq!(
                match_body.matches("NativeHandlerV1::").count(),
                owned_handlers.len(),
                "{family} compiler must not enumerate foreign handlers"
            );
            assert!(
                match_body.contains("\n        _ =>"),
                "{family} compiler must use a stable foreign-handler fallback"
            );
        }
    }

    #[test]
    fn configuration_compiler_is_registered() {
        assert_eq!(
            crate::rules::families::compiler("configuration").map(RuleFamilyCompiler::id),
            Some("configuration")
        );
    }

    #[test]
    fn empty_facts_helper_stays_strict() {
        assert_eq!(empty_facts()["components"], json!({}));
    }
}
