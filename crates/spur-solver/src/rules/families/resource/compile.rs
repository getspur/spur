//! Resource fact validation and lowering to typed solver constraints.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    rules::{
        compiler::{FamilyCompilation, FamilyCompileError, ModelProjection, RuleFamilyCompiler},
        manifest::{manifest_rule_handler, validate_binding_contract},
        manifest_family_executable_rule_ids,
        manifest_format::NativeHandlerV1,
        primitives::{add, and, boolean, eq, ge, gt, int, le, mul, or, request, sub, var},
        CompiledRule, RuleSolveMode,
    },
    types::{
        ConstraintExpr, Variable, DEFAULT_TIMEOUT_MS, MAX_CONSTRAINTS, MAX_TIMEOUT_MS,
        MAX_VARIABLES,
    },
};

/// Resource compiler registered behind `solve_rules`.
pub static COMPILER: ResourceCompiler = ResourceCompiler;

/// Stateless resource family compiler.
pub struct ResourceCompiler;

impl RuleFamilyCompiler for ResourceCompiler {
    fn id(&self) -> &'static str {
        "resource"
    }

    fn input_schema(&self) -> Value {
        input_schema()
    }

    fn compile(&self, input: Value) -> Result<FamilyCompilation, FamilyCompileError> {
        compile(input).map_err(|message| FamilyCompileError::new(self.id(), message))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceRequest {
    #[serde(rename = "family")]
    _family: String,
    mode: RuleSolveMode,
    rules: Vec<ResourceRuleBinding>,
    facts: ResourceFacts,
    #[serde(default)]
    unknowns: Vec<ResourceUnknown>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    persist: bool,
    #[serde(default)]
    include_smt: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceRuleBinding {
    rule_id: String,
    subjects: Vec<String>,
    #[serde(default)]
    parameters: ResourceParameters,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ResourceParameters {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    resources: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_skew: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    minimum_domains: Option<i64>,
}

struct ValidatedResourceBinding<'a> {
    source: &'a ResourceRuleBinding,
    handler: NativeHandlerV1,
    parameters: ResourceParameters,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceFacts {
    workloads: BTreeMap<String, WorkloadFacts>,
    pools: BTreeMap<String, CapacityFacts>,
    quotas: BTreeMap<String, CapacityFacts>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkloadFacts {
    replicas: Option<i64>,
    #[serde(default)]
    requests: BTreeMap<String, Option<i64>>,
    #[serde(default)]
    limits: BTreeMap<String, Option<i64>>,
    #[serde(default)]
    domain_counts: BTreeMap<String, Option<i64>>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct CapacityFacts {
    resources: BTreeMap<String, i64>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceUnknown {
    subject: String,
    field: String,
    min: i64,
    max: i64,
}

fn compile(input: Value) -> Result<FamilyCompilation, String> {
    let input: ResourceRequest =
        serde_json::from_value(input).map_err(|error| error.to_string())?;
    if input.rules.is_empty() {
        return Err("at least one resource rule binding is required".to_owned());
    }
    if input.rules.len() > MAX_CONSTRAINTS {
        return Err(format!(
            "request has too many resource rule bindings; maximum is {MAX_CONSTRAINTS}"
        ));
    }
    if input.mode == RuleSolveMode::Verify && !input.unknowns.is_empty() {
        return Err("verification requires complete resource facts".to_owned());
    }
    let bindings = input
        .rules
        .iter()
        .map(validate_manifest_binding)
        .collect::<Result<Vec<_>, _>>()?;
    validate_facts(&input.facts, &input.unknowns)?;
    let mut resolver = ResourceResolver::new(input.facts, &input.unknowns);
    let mut rules = Vec::with_capacity(input.rules.len());
    for (index, binding) in bindings.iter().enumerate() {
        let predicate = compile_binding(binding, &mut resolver)?;
        rules.push(CompiledRule::new(
            binding.source.rule_id.clone(),
            index,
            predicate,
        ));
    }
    let solver_request = request(
        "resource",
        resolver.variables,
        &rules,
        input.timeout_ms,
        input.persist,
        input.include_smt,
    );
    solver_request
        .validate()
        .map_err(|error| format!("compiled resource rules are invalid: {error}"))?;

    Ok(FamilyCompilation {
        mode: input.mode,
        request: solver_request,
        rules,
        projections: resolver.projections,
    })
}

fn validate_manifest_binding(
    binding: &ResourceRuleBinding,
) -> Result<ValidatedResourceBinding<'_>, String> {
    let parameters =
        serde_json::to_value(&binding.parameters).map_err(|error| error.to_string())?;
    let Value::Object(parameters) = parameters else {
        return Err("resource parameters did not serialize as an object".to_owned());
    };
    let validated = validate_binding_contract(&binding.rule_id, &binding.subjects, &parameters)
        .map_err(|message| stable_contract_error(binding, message))?;
    let parameters = serde_json::from_value(Value::Object(validated.parameters))
        .map_err(|error| error.to_string())?;

    Ok(ValidatedResourceBinding {
        source: binding,
        handler: validated.handler,
        parameters,
    })
}

fn stable_contract_error(binding: &ResourceRuleBinding, message: String) -> String {
    match validate_legacy_static_contract(binding) {
        Err(error) => error,
        Ok(()) => message,
    }
}

// The shared manifest validator owns acceptance. This error-only replay keeps the
// established resource diagnostics while the shared contract returns strings.
fn validate_legacy_static_contract(binding: &ResourceRuleBinding) -> Result<(), String> {
    match manifest_rule_handler(&binding.rule_id) {
        Some(NativeHandlerV1::ResourceRequestWithinLimit) => {
            require_subjects(binding, 1, None)?;
            reject_placement_parameters(binding)
        }
        Some(
            NativeHandlerV1::ResourceAggregateCapacity | NativeHandlerV1::ResourceQuotaCapacity,
        ) => {
            require_subjects(binding, 2, Some(usize::MAX))?;
            reject_placement_parameters(binding)
        }
        Some(NativeHandlerV1::PlacementTopologyMaxSkew) => {
            require_subjects(binding, 1, None)?;
            reject_resources(binding)?;
            reject_minimum_domains(binding)?;
            positive("max_skew", binding.parameters.max_skew.unwrap_or(1))?;
            Ok(())
        }
        Some(NativeHandlerV1::PlacementMinimumFailureDomains) => {
            require_subjects(binding, 1, None)?;
            reject_resources(binding)?;
            reject_max_skew(binding)?;
            positive(
                "minimum_domains",
                binding.parameters.minimum_domains.unwrap_or(1),
            )?;
            Ok(())
        }
        Some(_) | None => Err(format!("unsupported resource rule `{}`", binding.rule_id)),
    }
}

fn validate_facts(facts: &ResourceFacts, unknowns: &[ResourceUnknown]) -> Result<(), String> {
    if facts.workloads.len() > MAX_CONSTRAINTS
        || facts.pools.len() > MAX_CONSTRAINTS
        || facts.quotas.len() > MAX_CONSTRAINTS
    {
        return Err("resource facts exceed family limits".to_owned());
    }
    if unknowns.len() > MAX_VARIABLES {
        return Err(format!("resource unknown maximum is {MAX_VARIABLES}"));
    }
    for (workload, facts_for_workload) in &facts.workloads {
        if facts_for_workload.replicas.is_some_and(|value| value < 0) {
            return Err(format!(
                "workload `{workload}` replicas must be non-negative"
            ));
        }
        for (kind, values) in [
            ("requests", &facts_for_workload.requests),
            ("limits", &facts_for_workload.limits),
            ("domain_counts", &facts_for_workload.domain_counts),
        ] {
            for (name, value) in values {
                if value.is_some_and(|value| value < 0) {
                    return Err(format!(
                        "workload `{workload}` {kind}.{name} must be non-negative"
                    ));
                }
            }
        }
    }
    for (kind, capacities) in [("pool", &facts.pools), ("quota", &facts.quotas)] {
        for (subject, capacity) in capacities {
            for (resource, value) in &capacity.resources {
                if *value < 0 {
                    return Err(format!(
                        "{kind} `{subject}` resource `{resource}` must be non-negative"
                    ));
                }
            }
        }
    }

    let mut paths = BTreeSet::new();
    for unknown in unknowns {
        if unknown.min < 0 || unknown.min > unknown.max {
            return Err(format!(
                "unknown {}.{} must have 0 <= min <= max",
                unknown.subject, unknown.field
            ));
        }
        let workload = facts
            .workloads
            .get(&unknown.subject)
            .ok_or_else(|| format!("unknown resource workload `{}`", unknown.subject))?;
        let fixed = workload_value(workload, &unknown.field)?;
        if fixed.is_some() {
            return Err(format!(
                "resource field {}.{} is already fixed",
                unknown.subject, unknown.field
            ));
        }
        if !paths.insert((unknown.subject.clone(), unknown.field.clone())) {
            return Err(format!(
                "duplicate resource unknown {}.{}",
                unknown.subject, unknown.field
            ));
        }
    }
    Ok(())
}

fn workload_value(workload: &WorkloadFacts, field: &str) -> Result<Option<i64>, String> {
    if field == "replicas" {
        return Ok(workload.replicas);
    }
    let (group, name) = field
        .split_once('.')
        .ok_or_else(|| format!("unsupported resource field `{field}`"))?;
    let values = match group {
        "requests" => &workload.requests,
        "limits" => &workload.limits,
        "domain_counts" => &workload.domain_counts,
        _ => return Err(format!("unsupported resource field `{field}`")),
    };
    values
        .get(name)
        .copied()
        .ok_or_else(|| format!("resource field `{field}` is not declared"))
}

fn compile_binding(
    binding: &ValidatedResourceBinding<'_>,
    resolver: &mut ResourceResolver,
) -> Result<ConstraintExpr, String> {
    let source = binding.source;
    match binding.handler {
        NativeHandlerV1::ResourceRequestWithinLimit => {
            let mut predicates = Vec::new();
            for workload in distinct_subjects(&source.subjects) {
                resolver.require_workload(workload)?;
                let resources = selected_resources(
                    &binding.parameters.resources,
                    resolver.facts.workloads[workload].requests.keys(),
                )?;
                for resource in resources {
                    predicates.push(le(
                        resolver.workload_field(workload, &format!("requests.{resource}"))?,
                        resolver.workload_field(workload, &format!("limits.{resource}"))?,
                    ));
                }
            }
            Ok(conjunction(predicates))
        }
        NativeHandlerV1::ResourceAggregateCapacity => {
            compile_capacity(binding, resolver, CapacityKind::Pool)
        }
        NativeHandlerV1::ResourceQuotaCapacity => {
            compile_capacity(binding, resolver, CapacityKind::Quota)
        }
        NativeHandlerV1::PlacementTopologyMaxSkew => {
            let max_skew = binding
                .parameters
                .max_skew
                .expect("manifest defaults max_skew");
            let mut predicates = Vec::new();
            for workload in distinct_subjects(&source.subjects) {
                predicates.extend(topology_max_skew(resolver, workload, max_skew)?);
            }
            Ok(conjunction(predicates))
        }
        NativeHandlerV1::PlacementMinimumFailureDomains => {
            let minimum = binding
                .parameters
                .minimum_domains
                .expect("manifest defaults minimum_domains");
            let mut links = Vec::new();
            for workload in distinct_subjects(&source.subjects) {
                resolver.require_workload(workload)?;
                let domains = resolver.facts.workloads[workload]
                    .domain_counts
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>();
                links.push(placement_conservation(resolver, workload)?);
                let mut present = Vec::new();
                for domain in domains {
                    let count =
                        resolver.workload_field(workload, &format!("domain_counts.{domain}"))?;
                    if let Some(value) = resolver.facts.workloads[workload].domain_counts[&domain] {
                        present.push(int(i64::from(value > 0)));
                    } else {
                        let flag = resolver.presence(workload, &domain)?;
                        links.push(or(vec![
                            and(vec![eq(count.clone(), int(0)), eq(flag.clone(), int(0))]),
                            and(vec![gt(count, int(0)), eq(flag.clone(), int(1))]),
                        ]));
                        present.push(flag);
                    }
                }
                links.push(ge(sum(present), int(minimum)));
            }
            Ok(conjunction(links))
        }
        _ => Err(format!("unsupported resource rule `{}`", source.rule_id)),
    }
}

fn distinct_subjects(subjects: &[String]) -> Vec<&str> {
    let mut seen = BTreeSet::new();
    subjects
        .iter()
        .filter_map(|subject| seen.insert(subject.as_str()).then_some(subject.as_str()))
        .collect()
}

fn topology_max_skew(
    resolver: &ResourceResolver,
    workload: &str,
    max_skew: i64,
) -> Result<Vec<ConstraintExpr>, String> {
    resolver.require_workload(workload)?;
    let domains = resolver.facts.workloads[workload]
        .domain_counts
        .iter()
        .map(|(domain, value)| (domain.clone(), *value))
        .collect::<Vec<_>>();
    let mut fixed_minimum = None;
    let mut fixed_maximum = None;
    let mut unknown_counts = Vec::new();
    for (domain, value) in domains {
        if let Some(value) = value {
            fixed_minimum = Some(fixed_minimum.map_or(value, |minimum: i64| minimum.min(value)));
            fixed_maximum = Some(fixed_maximum.map_or(value, |maximum: i64| maximum.max(value)));
        } else {
            unknown_counts
                .push(resolver.workload_field(workload, &format!("domain_counts.{domain}"))?);
        }
    }

    let mut predicates = vec![placement_conservation(resolver, workload)?];
    if let (Some(minimum), Some(maximum)) = (fixed_minimum, fixed_maximum) {
        predicates.push(le(int(maximum - minimum), int(max_skew)));
        for count in &unknown_counts {
            predicates.push(le(sub(count.clone(), int(minimum)), int(max_skew)));
            predicates.push(le(sub(int(maximum), count.clone()), int(max_skew)));
        }
    }
    for left in 0..unknown_counts.len() {
        for right in left + 1..unknown_counts.len() {
            predicates.push(le(
                sub(unknown_counts[left].clone(), unknown_counts[right].clone()),
                int(max_skew),
            ));
            predicates.push(le(
                sub(unknown_counts[right].clone(), unknown_counts[left].clone()),
                int(max_skew),
            ));
        }
    }
    Ok(predicates)
}

enum CapacityKind {
    Pool,
    Quota,
}

fn compile_capacity(
    binding: &ValidatedResourceBinding<'_>,
    resolver: &ResourceResolver,
    kind: CapacityKind,
) -> Result<ConstraintExpr, String> {
    let source = binding.source;
    let capacity_id = &source.subjects[0];
    let (capacity, capacity_kind) = match kind {
        CapacityKind::Pool => (
            resolver
                .facts
                .pools
                .get(capacity_id)
                .ok_or_else(|| format!("unknown resource pool `{capacity_id}`"))?,
            "pool",
        ),
        CapacityKind::Quota => (
            resolver
                .facts
                .quotas
                .get(capacity_id)
                .ok_or_else(|| format!("unknown resource quota `{capacity_id}`"))?,
            "quota",
        ),
    };
    for workload in &source.subjects[1..] {
        resolver.require_workload(workload)?;
    }
    let resources = selected_resources(&binding.parameters.resources, capacity.resources.keys())?;
    for resource in &resources {
        if !capacity.resources.contains_key(resource) {
            return Err(format!(
                "{capacity_kind} `{capacity_id}` does not declare resource `{resource}`"
            ));
        }
    }
    let predicates = resources
        .iter()
        .map(|resource| {
            let demand = source.subjects[1..]
                .iter()
                .map(|workload| {
                    Ok(mul(vec![
                        resolver.workload_field(workload, "replicas")?,
                        resolver.workload_field(workload, &format!("requests.{resource}"))?,
                    ]))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(le(sum(demand), int(capacity.resources[resource])))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(conjunction(predicates))
}

fn placement_conservation(
    resolver: &ResourceResolver,
    workload: &str,
) -> Result<ConstraintExpr, String> {
    let domain_counts = resolver.facts.workloads[workload]
        .domain_counts
        .keys()
        .map(|domain| resolver.workload_field(workload, &format!("domain_counts.{domain}")))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(eq(
        sum(domain_counts),
        resolver.workload_field(workload, "replicas")?,
    ))
}

fn selected_resources<'a>(
    selected: &[String],
    defaults: impl Iterator<Item = &'a String>,
) -> Result<Vec<String>, String> {
    let resources = if selected.is_empty() {
        defaults.cloned().collect::<Vec<_>>()
    } else {
        selected.to_vec()
    };
    if resources.is_empty() {
        return Err("resource rule requires at least one named resource".to_owned());
    }
    let unique = resources.iter().collect::<BTreeSet<_>>();
    if unique.len() != resources.len() {
        return Err("resource selection contains duplicates".to_owned());
    }
    Ok(resources)
}

fn require_subjects(
    binding: &ResourceRuleBinding,
    minimum: usize,
    maximum: Option<usize>,
) -> Result<(), String> {
    let valid = binding.subjects.len() >= minimum
        && maximum.is_none_or(|maximum| binding.subjects.len() <= maximum);
    if !valid {
        return Err(format!(
            "rule `{}` requires at least {minimum} subjects, got {}",
            binding.rule_id,
            binding.subjects.len()
        ));
    }
    Ok(())
}

fn reject_placement_parameters(binding: &ResourceRuleBinding) -> Result<(), String> {
    reject_max_skew(binding)?;
    reject_minimum_domains(binding)
}

fn reject_resources(binding: &ResourceRuleBinding) -> Result<(), String> {
    if !binding.parameters.resources.is_empty() {
        return Err(format!(
            "rule `{}` does not accept resources",
            binding.rule_id
        ));
    }
    Ok(())
}

fn reject_max_skew(binding: &ResourceRuleBinding) -> Result<(), String> {
    if binding.parameters.max_skew.is_some() {
        return Err(format!(
            "rule `{}` does not accept max_skew",
            binding.rule_id
        ));
    }
    Ok(())
}

fn reject_minimum_domains(binding: &ResourceRuleBinding) -> Result<(), String> {
    if binding.parameters.minimum_domains.is_some() {
        return Err(format!(
            "rule `{}` does not accept minimum_domains",
            binding.rule_id
        ));
    }
    Ok(())
}

fn positive(parameter: &str, value: i64) -> Result<i64, String> {
    if value <= 0 {
        return Err(format!("`{parameter}` must be positive"));
    }
    Ok(value)
}

fn conjunction(predicates: Vec<ConstraintExpr>) -> ConstraintExpr {
    match predicates.len() {
        0 => boolean(true),
        1 => predicates.into_iter().next().expect("one predicate"),
        _ => and(predicates),
    }
}

fn sum(expressions: Vec<ConstraintExpr>) -> ConstraintExpr {
    match expressions.len() {
        0 => int(0),
        1 => expressions.into_iter().next().expect("one expression"),
        _ => add(expressions),
    }
}

struct ResourceResolver {
    facts: ResourceFacts,
    paths: BTreeMap<(String, String), String>,
    presence_paths: BTreeMap<(String, String), String>,
    variables: Vec<Variable>,
    projections: Vec<ModelProjection>,
}

impl ResourceResolver {
    fn new(facts: ResourceFacts, unknowns: &[ResourceUnknown]) -> Self {
        let mut sorted = unknowns.to_vec();
        sorted.sort_by(|left, right| {
            (&left.subject, &left.field).cmp(&(&right.subject, &right.field))
        });
        let mut paths = BTreeMap::new();
        let mut variables = Vec::new();
        let mut projections = Vec::new();
        for (index, unknown) in sorted.into_iter().enumerate() {
            let variable = format!("resource_u_{index}");
            paths.insert(
                (unknown.subject.clone(), unknown.field.clone()),
                variable.clone(),
            );
            variables.push(Variable::IntRange {
                name: variable.clone(),
                min: unknown.min,
                max: unknown.max,
            });
            projections.push(ModelProjection {
                variable,
                subject: unknown.subject,
                field: unknown.field,
            });
        }
        Self {
            facts,
            paths,
            presence_paths: BTreeMap::new(),
            variables,
            projections,
        }
    }

    fn require_workload(&self, workload: &str) -> Result<(), String> {
        if !self.facts.workloads.contains_key(workload) {
            return Err(format!("unknown resource workload `{workload}`"));
        }
        Ok(())
    }

    fn workload_field(&self, workload: &str, field: &str) -> Result<ConstraintExpr, String> {
        self.require_workload(workload)?;
        if let Some(value) = workload_value(&self.facts.workloads[workload], field)? {
            return Ok(int(value));
        }
        self.paths
            .get(&(workload.to_owned(), field.to_owned()))
            .cloned()
            .map(var)
            .ok_or_else(|| format!("{workload}.{field} is null and has no unknown declaration"))
    }

    fn presence(&mut self, workload: &str, domain: &str) -> Result<ConstraintExpr, String> {
        let key = (workload.to_owned(), domain.to_owned());
        if let Some(name) = self.presence_paths.get(&key) {
            return Ok(var(name.clone()));
        }
        let name = format!("resource_presence_{}", self.presence_paths.len());
        if self.variables.len() >= MAX_VARIABLES {
            return Err(format!("resource variables exceed maximum {MAX_VARIABLES}"));
        }
        self.variables.push(Variable::IntRange {
            name: name.clone(),
            min: 0,
            max: 1,
        });
        self.presence_paths.insert(key, name.clone());
        Ok(var(name))
    }
}

const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn input_schema() -> Value {
    let rule_ids = manifest_family_executable_rule_ids("resource")
        .expect("resource manifest executable rule IDs");
    let nullable_non_negative = json!({"type": ["integer", "null"], "minimum": 0});
    let nullable_resource_map = json!({"type": "object", "maxProperties": MAX_CONSTRAINTS, "additionalProperties": nullable_non_negative});
    let capacity_map = json!({
        "type": "object", "maxProperties": MAX_CONSTRAINTS,
        "additionalProperties": {
            "type": "object",
            "properties": {"resources": {"type": "object", "maxProperties": MAX_CONSTRAINTS, "additionalProperties": {"type": "integer", "minimum": 0}}},
            "required": ["resources"], "additionalProperties": false
        }
    });
    json!({
        "type": "object",
        "properties": {
            "family": {"const": "resource"},
            "mode": {"type": "string", "enum": ["verify", "synthesize"]},
            "rules": {
                "type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS,
                "items": {
                    "type": "object",
                    "properties": {
                        "rule_id": {"type": "string", "enum": rule_ids},
                        "subjects": {"type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS, "items": {"type": "string"}},
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "resources": {"type": "array", "maxItems": MAX_CONSTRAINTS, "items": {"type": "string"}},
                                "max_skew": {"type": "integer", "minimum": 1},
                                "minimum_domains": {"type": "integer", "minimum": 1}
                            },
                            "additionalProperties": false
                        }
                    },
                    "required": ["rule_id", "subjects"], "additionalProperties": false
                }
            },
            "facts": {
                "type": "object",
                "properties": {
                    "workloads": {
                        "type": "object", "maxProperties": MAX_CONSTRAINTS,
                        "additionalProperties": {
                            "type": "object",
                            "properties": {
                                "replicas": {"type": ["integer", "null"], "minimum": 0},
                                "requests": nullable_resource_map,
                                "limits": nullable_resource_map,
                                "domain_counts": nullable_resource_map
                            },
                            "required": ["replicas"], "additionalProperties": false
                        }
                    },
                    "pools": capacity_map,
                    "quotas": capacity_map
                },
                "required": ["workloads", "pools", "quotas"], "additionalProperties": false
            },
            "unknowns": {
                "type": "array", "maxItems": MAX_VARIABLES, "default": [],
                "items": {
                    "type": "object",
                    "properties": {
                        "subject": {"type": "string"}, "field": {"type": "string"},
                        "min": {"type": "integer", "minimum": 0}, "max": {"type": "integer", "minimum": 0}
                    },
                    "required": ["subject", "field", "min", "max"], "additionalProperties": false
                }
            },
            "timeout_ms": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_MS, "default": DEFAULT_TIMEOUT_MS},
            "persist": {"type": "boolean", "default": false},
            "include_smt": {"type": "boolean", "default": false}
        },
        "required": ["family", "mode", "rules", "facts"], "additionalProperties": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expression_nodes(expression: &ConstraintExpr) -> usize {
        match expression {
            ConstraintExpr::Op { args, .. } => 1 + args.iter().map(expression_nodes).sum::<usize>(),
            ConstraintExpr::Var { .. }
            | ConstraintExpr::Int { .. }
            | ConstraintExpr::Bool { .. }
            | ConstraintExpr::EnumLabel { .. }
            | ConstraintExpr::Real { .. }
            | ConstraintExpr::Bv { .. } => 1,
        }
    }

    #[test]
    fn fixed_topology_skew_compiles_linearly_at_the_domain_limit() {
        let rule_id = manifest_family_executable_rule_ids("resource")
            .expect("resource rules")
            .iter()
            .find(|rule_id| {
                manifest_rule_handler(rule_id.as_str())
                    == Some(NativeHandlerV1::PlacementTopologyMaxSkew)
            })
            .expect("topology max-skew rule");
        let domains = (0..MAX_CONSTRAINTS)
            .map(|index| (format!("zone-{index}"), json!(1)))
            .collect::<serde_json::Map<_, _>>();
        let compilation = compile(json!({
            "family": "resource",
            "mode": "verify",
            "rules": [{
                "rule_id": rule_id,
                "subjects": ["workload"],
                "parameters": {"max_skew": 1}
            }],
            "facts": {
                "workloads": {
                    "workload": {
                        "replicas": MAX_CONSTRAINTS,
                        "requests": {},
                        "limits": {},
                        "domain_counts": Value::Object(domains)
                    }
                },
                "pools": {},
                "quotas": {}
            },
            "unknowns": []
        }))
        .expect("boundary topology fixture compiles");

        let nodes = expression_nodes(&compilation.rules[0].predicate);
        assert!(
            nodes <= MAX_CONSTRAINTS * 2,
            "fixed topology compilation must be linear, found {nodes} nodes"
        );
    }
}
