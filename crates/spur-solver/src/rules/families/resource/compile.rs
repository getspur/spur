//! Resource fact validation and lowering to typed solver constraints.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    rules::{
        compiler::{FamilyCompilation, FamilyCompileError, ModelProjection, RuleFamilyCompiler},
        primitives::{add, and, boolean, eq, ge, gt, int, le, mul, or, request, sub, var},
        CompiledRule, RuleSolveMode,
    },
    types::{
        ConstraintExpr, Variable, DEFAULT_TIMEOUT_MS, MAX_CONSTRAINTS, MAX_TIMEOUT_MS,
        MAX_VARIABLES,
    },
};

use super::builtin_registry;

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

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceParameters {
    #[serde(default)]
    resources: Vec<String>,
    max_skew: Option<i64>,
    minimum_domains: Option<i64>,
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
    validate_facts(&input.facts, &input.unknowns)?;
    let mut resolver = ResourceResolver::new(input.facts, &input.unknowns);
    let mut rules = Vec::with_capacity(input.rules.len());
    for (index, binding) in input.rules.iter().enumerate() {
        let predicate = compile_binding(binding, &mut resolver)?;
        rules.push(CompiledRule::new(binding.rule_id.clone(), index, predicate));
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
    binding: &ResourceRuleBinding,
    resolver: &mut ResourceResolver,
) -> Result<ConstraintExpr, String> {
    if builtin_registry().rule(&binding.rule_id).is_none() {
        return Err(format!("unsupported resource rule `{}`", binding.rule_id));
    }
    match binding.rule_id.as_str() {
        "resource.request_within_limit" => {
            require_subjects(binding, 1, None)?;
            reject_placement_parameters(binding)?;
            let workload = &binding.subjects[0];
            resolver.require_workload(workload)?;
            let resources = selected_resources(
                &binding.parameters.resources,
                resolver.facts.workloads[workload].requests.keys(),
            )?;
            let predicates = resources
                .iter()
                .map(|resource| {
                    Ok(le(
                        resolver.workload_field(workload, &format!("requests.{resource}"))?,
                        resolver.workload_field(workload, &format!("limits.{resource}"))?,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(conjunction(predicates))
        }
        "resource.aggregate_capacity" => {
            require_subjects(binding, 2, Some(usize::MAX))?;
            reject_placement_parameters(binding)?;
            compile_capacity(binding, resolver, CapacityKind::Pool)
        }
        "resource.quota_capacity" => {
            require_subjects(binding, 2, Some(usize::MAX))?;
            reject_placement_parameters(binding)?;
            compile_capacity(binding, resolver, CapacityKind::Quota)
        }
        "placement.topology_max_skew" => {
            require_subjects(binding, 1, None)?;
            reject_resources(binding)?;
            reject_minimum_domains(binding)?;
            let max_skew = non_negative("max_skew", binding.parameters.max_skew.unwrap_or(1))?;
            let workload = &binding.subjects[0];
            resolver.require_workload(workload)?;
            let domains = resolver.facts.workloads[workload]
                .domain_counts
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            let mut predicates = Vec::new();
            for left in 0..domains.len() {
                for right in left + 1..domains.len() {
                    let left_count = resolver
                        .workload_field(workload, &format!("domain_counts.{}", domains[left]))?;
                    let right_count = resolver
                        .workload_field(workload, &format!("domain_counts.{}", domains[right]))?;
                    predicates.push(le(
                        sub(left_count.clone(), right_count.clone()),
                        int(max_skew),
                    ));
                    predicates.push(le(sub(right_count, left_count), int(max_skew)));
                }
            }
            Ok(conjunction(predicates))
        }
        "placement.minimum_failure_domains" => {
            require_subjects(binding, 1, None)?;
            reject_resources(binding)?;
            reject_max_skew(binding)?;
            let minimum = positive(
                "minimum_domains",
                binding.parameters.minimum_domains.unwrap_or(1),
            )?;
            let workload = &binding.subjects[0];
            resolver.require_workload(workload)?;
            let domains = resolver.facts.workloads[workload]
                .domain_counts
                .keys()
                .cloned()
                .collect::<Vec<_>>();
            let mut links = Vec::new();
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
            Ok(conjunction(links))
        }
        _ => Err(format!("unsupported resource rule `{}`", binding.rule_id)),
    }
}

enum CapacityKind {
    Pool,
    Quota,
}

fn compile_capacity(
    binding: &ResourceRuleBinding,
    resolver: &ResourceResolver,
    kind: CapacityKind,
) -> Result<ConstraintExpr, String> {
    let capacity_id = &binding.subjects[0];
    let capacity = match kind {
        CapacityKind::Pool => resolver
            .facts
            .pools
            .get(capacity_id)
            .ok_or_else(|| format!("unknown resource pool `{capacity_id}`"))?,
        CapacityKind::Quota => resolver
            .facts
            .quotas
            .get(capacity_id)
            .ok_or_else(|| format!("unknown resource quota `{capacity_id}`"))?,
    };
    for workload in &binding.subjects[1..] {
        resolver.require_workload(workload)?;
    }
    let resources = selected_resources(&binding.parameters.resources, capacity.resources.keys())?;
    let predicates = resources
        .iter()
        .map(|resource| {
            let demand = binding.subjects[1..]
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

fn non_negative(parameter: &str, value: i64) -> Result<i64, String> {
    if value < 0 {
        return Err(format!("`{parameter}` must be non-negative"));
    }
    Ok(value)
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
                        "rule_id": {"type": "string", "enum": [
                            "placement.minimum_failure_domains", "placement.topology_max_skew",
                            "resource.aggregate_capacity", "resource.quota_capacity", "resource.request_within_limit"
                        ]},
                        "subjects": {"type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS, "items": {"type": "string"}},
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "resources": {"type": "array", "maxItems": MAX_CONSTRAINTS, "items": {"type": "string"}},
                                "max_skew": {"type": "integer", "minimum": 0},
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
