//! Finite RBAC validation and lowering to typed solver constraints.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    rules::{
        compiler::{FamilyCompilation, FamilyCompileError, ModelProjection, RuleFamilyCompiler},
        primitives::{add, and, boolean, eq, int, le, lt, or, request, var},
        CompiledRule, RuleSolveMode,
    },
    types::{
        ConstraintExpr, Variable, DEFAULT_TIMEOUT_MS, MAX_CONSTRAINTS, MAX_TIMEOUT_MS,
        MAX_VARIABLES,
    },
};

use super::builtin_registry;

/// Policy compiler registered behind `solve_rules`.
pub static COMPILER: PolicyCompiler = PolicyCompiler;

/// Stateless finite-RBAC compiler.
pub struct PolicyCompiler;

impl RuleFamilyCompiler for PolicyCompiler {
    fn id(&self) -> &'static str {
        "policy"
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
struct PolicyRequest {
    #[serde(rename = "family")]
    _family: String,
    mode: RuleSolveMode,
    rules: Vec<PolicyRuleBinding>,
    facts: PolicyFacts,
    #[serde(default)]
    unknowns: Vec<PolicyUnknown>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    persist: bool,
    #[serde(default)]
    include_smt: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyRuleBinding {
    rule_id: String,
    subjects: Vec<String>,
    #[serde(default)]
    parameters: PolicyParameters,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyParameters {
    #[serde(default)]
    roles: Vec<String>,
    max_assigned: Option<i64>,
    max_active: Option<i64>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFacts {
    roles: BTreeMap<String, RoleFacts>,
    principals: BTreeMap<String, PrincipalFacts>,
    sessions: BTreeMap<String, SessionFacts>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleFacts {
    #[serde(default)]
    inherits: Vec<String>,
    #[serde(default)]
    permissions: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrincipalFacts {
    #[serde(default)]
    roles: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionFacts {
    principal: String,
    #[serde(default)]
    active_roles: Vec<String>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PolicyUnknown {
    PrincipalRole { principal: String, role: String },
    SessionRole { session: String, role: String },
}

fn compile(input: Value) -> Result<FamilyCompilation, String> {
    let input: PolicyRequest = serde_json::from_value(input).map_err(|error| error.to_string())?;
    if input.rules.is_empty() {
        return Err("at least one policy rule binding is required".to_owned());
    }
    if input.rules.len() > MAX_CONSTRAINTS {
        return Err(format!(
            "request has too many policy rule bindings; maximum is {MAX_CONSTRAINTS}"
        ));
    }
    if input.mode == RuleSolveMode::Verify && !input.unknowns.is_empty() {
        return Err("verification requires complete policy memberships".to_owned());
    }
    validate_facts(&input.facts, &input.unknowns)?;
    let mut resolver = PolicyResolver::new(input.facts, &input.unknowns);
    let mut rules = Vec::with_capacity(input.rules.len());
    for (index, binding) in input.rules.iter().enumerate() {
        let predicate = compile_binding(binding, &mut resolver)?;
        rules.push(CompiledRule::new(binding.rule_id.clone(), index, predicate));
    }
    let solver_request = request(
        "policy",
        resolver.variables,
        &rules,
        input.timeout_ms,
        input.persist,
        input.include_smt,
    );
    solver_request
        .validate()
        .map_err(|error| format!("compiled policy rules are invalid: {error}"))?;

    Ok(FamilyCompilation {
        mode: input.mode,
        request: solver_request,
        rules,
        projections: resolver.projections,
    })
}

fn validate_facts(facts: &PolicyFacts, unknowns: &[PolicyUnknown]) -> Result<(), String> {
    if facts.roles.len() > MAX_VARIABLES
        || facts.principals.len() > MAX_CONSTRAINTS
        || facts.sessions.len() > MAX_CONSTRAINTS
    {
        return Err("policy facts exceed solver family limits".to_owned());
    }
    if unknowns.len() > MAX_VARIABLES {
        return Err(format!("policy unknown maximum is {MAX_VARIABLES}"));
    }
    for (role, facts_for_role) in &facts.roles {
        reject_duplicates(
            &facts_for_role.inherits,
            &format!("role `{role}` inheritance"),
        )?;
        reject_duplicates(
            &facts_for_role.permissions,
            &format!("role `{role}` permissions"),
        )?;
        for inherited in &facts_for_role.inherits {
            if !facts.roles.contains_key(inherited) {
                return Err(format!("role `{role}` inherits unknown role `{inherited}`"));
            }
        }
    }
    for (principal, principal_facts) in &facts.principals {
        reject_duplicates(
            &principal_facts.roles,
            &format!("principal `{principal}` roles"),
        )?;
        for role in &principal_facts.roles {
            if !facts.roles.contains_key(role) {
                return Err(format!("principal `{principal}` has unknown role `{role}`"));
            }
        }
    }
    for (session, session_facts) in &facts.sessions {
        if !facts.principals.contains_key(&session_facts.principal) {
            return Err(format!(
                "session `{session}` references unknown principal `{}`",
                session_facts.principal
            ));
        }
        reject_duplicates(
            &session_facts.active_roles,
            &format!("session `{session}` active roles"),
        )?;
        for role in &session_facts.active_roles {
            if !facts.roles.contains_key(role) {
                return Err(format!("session `{session}` has unknown role `{role}`"));
            }
        }
    }

    let mut seen = BTreeSet::new();
    for unknown in unknowns {
        let key = match unknown {
            PolicyUnknown::PrincipalRole { principal, role } => {
                if !facts.principals.contains_key(principal) || !facts.roles.contains_key(role) {
                    return Err(format!(
                        "unknown principal-role membership `{principal}/{role}` references missing facts"
                    ));
                }
                if facts.principals[principal].roles.contains(role) {
                    return Err(format!(
                        "principal-role membership `{principal}/{role}` is already fixed true"
                    ));
                }
                format!("principal:{principal}:{role}")
            }
            PolicyUnknown::SessionRole { session, role } => {
                if !facts.sessions.contains_key(session) || !facts.roles.contains_key(role) {
                    return Err(format!(
                        "unknown session-role membership `{session}/{role}` references missing facts"
                    ));
                }
                if facts.sessions[session].active_roles.contains(role) {
                    return Err(format!(
                        "session-role membership `{session}/{role}` is already fixed true"
                    ));
                }
                format!("session:{session}:{role}")
            }
        };
        if !seen.insert(key.clone()) {
            return Err(format!("duplicate policy unknown `{key}`"));
        }
    }
    Ok(())
}

fn reject_duplicates(items: &[String], context: &str) -> Result<(), String> {
    let unique = items.iter().collect::<BTreeSet<_>>();
    if unique.len() != items.len() {
        return Err(format!("{context} contains duplicates"));
    }
    Ok(())
}

fn compile_binding(
    binding: &PolicyRuleBinding,
    resolver: &mut PolicyResolver,
) -> Result<ConstraintExpr, String> {
    if builtin_registry().rule(&binding.rule_id).is_none()
        || binding.rule_id == "rbac.minimum_privilege"
    {
        return Err(format!("unsupported policy rule `{}`", binding.rule_id));
    }
    match binding.rule_id.as_str() {
        "rbac.permission_reachable" => {
            require_subjects(binding, 2)?;
            reject_parameters(binding)?;
            let principal = &binding.subjects[0];
            if !resolver.facts.principals.contains_key(principal) {
                return Err(format!("unknown policy principal `{principal}`"));
            }
            let permission = &binding.subjects[1];
            let granting_roles = resolver.roles_reaching_permission(permission);
            let assignments = granting_roles
                .into_iter()
                .map(|role| eq(resolver.principal_role(principal, &role), int(1)))
                .collect::<Vec<_>>();
            Ok(if assignments.is_empty() {
                boolean(false)
            } else {
                or(assignments)
            })
        }
        "rbac.role_hierarchy_acyclic" => {
            require_subjects(binding, 0)?;
            reject_parameters(binding)?;
            let edges = resolver
                .facts
                .roles
                .iter()
                .flat_map(|(role, facts)| {
                    facts
                        .inherits
                        .iter()
                        .map(move |inherited| (role.clone(), inherited.clone()))
                })
                .collect::<Vec<_>>();
            resolver.ensure_ranks()?;
            let predicates = edges
                .into_iter()
                .map(|(role, inherited)| lt(resolver.rank(&inherited), resolver.rank(&role)))
                .collect::<Vec<_>>();
            Ok(if predicates.is_empty() {
                boolean(true)
            } else {
                and(predicates)
            })
        }
        "rbac.static_separation_of_duty" => {
            require_subjects(binding, 1)?;
            reject_max_active(binding)?;
            let principal = &binding.subjects[0];
            if !resolver.facts.principals.contains_key(principal) {
                return Err(format!("unknown policy principal `{principal}`"));
            }
            validate_parameter_roles(binding, &resolver.facts)?;
            let max = positive_limit("max_assigned", binding.parameters.max_assigned.unwrap_or(1))?;
            Ok(le(
                sum(binding
                    .parameters
                    .roles
                    .iter()
                    .map(|role| resolver.principal_role(principal, role))
                    .collect()),
                int(max),
            ))
        }
        "rbac.dynamic_separation_of_duty" => {
            require_subjects(binding, 1)?;
            reject_max_assigned(binding)?;
            let session = &binding.subjects[0];
            if !resolver.facts.sessions.contains_key(session) {
                return Err(format!("unknown policy session `{session}`"));
            }
            validate_parameter_roles(binding, &resolver.facts)?;
            let max = positive_limit("max_active", binding.parameters.max_active.unwrap_or(1))?;
            Ok(le(
                sum(binding
                    .parameters
                    .roles
                    .iter()
                    .map(|role| resolver.session_role(session, role))
                    .collect()),
                int(max),
            ))
        }
        _ => Err(format!("unsupported policy rule `{}`", binding.rule_id)),
    }
}

fn require_subjects(binding: &PolicyRuleBinding, expected: usize) -> Result<(), String> {
    if binding.subjects.len() != expected {
        return Err(format!(
            "rule `{}` requires {expected} subjects, got {}",
            binding.rule_id,
            binding.subjects.len()
        ));
    }
    Ok(())
}

fn validate_parameter_roles(
    binding: &PolicyRuleBinding,
    facts: &PolicyFacts,
) -> Result<(), String> {
    if binding.parameters.roles.is_empty() {
        return Err(format!("rule `{}` requires `roles`", binding.rule_id));
    }
    reject_duplicates(&binding.parameters.roles, "separation roles")?;
    for role in &binding.parameters.roles {
        if !facts.roles.contains_key(role) {
            return Err(format!("separation rule references unknown role `{role}`"));
        }
    }
    Ok(())
}

fn reject_parameters(binding: &PolicyRuleBinding) -> Result<(), String> {
    if !binding.parameters.roles.is_empty()
        || binding.parameters.max_assigned.is_some()
        || binding.parameters.max_active.is_some()
    {
        return Err(format!(
            "rule `{}` does not accept parameters",
            binding.rule_id
        ));
    }
    Ok(())
}

fn reject_max_active(binding: &PolicyRuleBinding) -> Result<(), String> {
    if binding.parameters.max_active.is_some() {
        return Err(format!(
            "rule `{}` does not accept `max_active`",
            binding.rule_id
        ));
    }
    Ok(())
}

fn reject_max_assigned(binding: &PolicyRuleBinding) -> Result<(), String> {
    if binding.parameters.max_assigned.is_some() {
        return Err(format!(
            "rule `{}` does not accept `max_assigned`",
            binding.rule_id
        ));
    }
    Ok(())
}

fn positive_limit(parameter: &str, value: i64) -> Result<i64, String> {
    if value <= 0 {
        return Err(format!("`{parameter}` must be positive"));
    }
    Ok(value)
}

fn sum(mut expressions: Vec<ConstraintExpr>) -> ConstraintExpr {
    match expressions.len() {
        0 => int(0),
        1 => expressions.remove(0),
        _ => add(expressions),
    }
}

struct PolicyResolver {
    facts: PolicyFacts,
    principal_unknowns: BTreeMap<(String, String), String>,
    session_unknowns: BTreeMap<(String, String), String>,
    ranks: BTreeMap<String, String>,
    variables: Vec<Variable>,
    projections: Vec<ModelProjection>,
}

impl PolicyResolver {
    fn new(facts: PolicyFacts, unknowns: &[PolicyUnknown]) -> Self {
        let mut principal_unknowns = BTreeMap::new();
        let mut session_unknowns = BTreeMap::new();
        let mut variables = Vec::new();
        let mut projections = Vec::new();
        let mut sorted = unknowns.to_vec();
        sorted.sort_by_key(PolicyUnknown::stable_key);
        for (index, unknown) in sorted.into_iter().enumerate() {
            let variable = format!("policy_u_{index}");
            variables.push(Variable::IntRange {
                name: variable.clone(),
                min: 0,
                max: 1,
            });
            match unknown {
                PolicyUnknown::PrincipalRole { principal, role } => {
                    principal_unknowns.insert((principal.clone(), role.clone()), variable.clone());
                    projections.push(ModelProjection {
                        variable,
                        subject: principal,
                        field: format!("roles.{role}"),
                    });
                }
                PolicyUnknown::SessionRole { session, role } => {
                    session_unknowns.insert((session.clone(), role.clone()), variable.clone());
                    projections.push(ModelProjection {
                        variable,
                        subject: session,
                        field: format!("active_roles.{role}"),
                    });
                }
            }
        }
        Self {
            facts,
            principal_unknowns,
            session_unknowns,
            ranks: BTreeMap::new(),
            variables,
            projections,
        }
    }

    fn principal_role(&self, principal: &str, role: &str) -> ConstraintExpr {
        self.principal_unknowns
            .get(&(principal.to_owned(), role.to_owned()))
            .cloned()
            .map_or_else(
                || {
                    int(i64::from(
                        self.facts.principals[principal]
                            .roles
                            .contains(&role.to_owned()),
                    ))
                },
                var,
            )
    }

    fn session_role(&self, session: &str, role: &str) -> ConstraintExpr {
        self.session_unknowns
            .get(&(session.to_owned(), role.to_owned()))
            .cloned()
            .map_or_else(
                || {
                    int(i64::from(
                        self.facts.sessions[session]
                            .active_roles
                            .contains(&role.to_owned()),
                    ))
                },
                var,
            )
    }

    fn roles_reaching_permission(&self, permission: &str) -> Vec<String> {
        self.facts
            .roles
            .keys()
            .filter(|role| self.role_reaches_permission(role, permission, &mut BTreeSet::new()))
            .cloned()
            .collect()
    }

    fn role_reaches_permission(
        &self,
        role: &str,
        permission: &str,
        visited: &mut BTreeSet<String>,
    ) -> bool {
        if !visited.insert(role.to_owned()) {
            return false;
        }
        let facts = &self.facts.roles[role];
        facts.permissions.iter().any(|item| item == permission)
            || facts
                .inherits
                .iter()
                .any(|inherited| self.role_reaches_permission(inherited, permission, visited))
    }

    fn ensure_ranks(&mut self) -> Result<(), String> {
        if !self.ranks.is_empty() || self.facts.roles.is_empty() {
            return Ok(());
        }
        let max = i64::try_from(self.facts.roles.len().saturating_sub(1))
            .map_err(|_error| "role count does not fit solver integer bounds".to_owned())?;
        for (index, role) in self.facts.roles.keys().enumerate() {
            let name = format!("policy_rank_{index}");
            self.variables.push(Variable::IntRange {
                name: name.clone(),
                min: 0,
                max,
            });
            self.ranks.insert(role.clone(), name);
        }
        Ok(())
    }

    fn rank(&self, role: &str) -> ConstraintExpr {
        var(self.ranks[role].clone())
    }
}

impl PolicyUnknown {
    fn stable_key(&self) -> String {
        match self {
            Self::PrincipalRole { principal, role } => format!("principal:{principal}:{role}"),
            Self::SessionRole { session, role } => format!("session:{session}:{role}"),
        }
    }
}

const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn input_schema() -> Value {
    let role_array =
        json!({"type": "array", "maxItems": MAX_VARIABLES, "items": {"type": "string"}});
    json!({
        "type": "object",
        "properties": {
            "family": {"const": "policy"},
            "mode": {"type": "string", "enum": ["verify", "synthesize"]},
            "rules": {
                "type": "array", "minItems": 1, "maxItems": MAX_CONSTRAINTS,
                "items": {
                    "type": "object",
                    "properties": {
                        "rule_id": {"type": "string", "enum": [
                            "rbac.dynamic_separation_of_duty", "rbac.permission_reachable",
                            "rbac.role_hierarchy_acyclic", "rbac.static_separation_of_duty"
                        ]},
                        "subjects": {"type": "array", "maxItems": 2, "items": {"type": "string"}},
                        "parameters": {
                            "type": "object",
                            "properties": {
                                "roles": role_array,
                                "max_assigned": {"type": "integer", "minimum": 1},
                                "max_active": {"type": "integer", "minimum": 1}
                            },
                            "additionalProperties": false
                        }
                    },
                    "required": ["rule_id", "subjects"],
                    "additionalProperties": false
                }
            },
            "facts": {
                "type": "object",
                "properties": {
                    "roles": {
                        "type": "object", "maxProperties": MAX_VARIABLES,
                        "additionalProperties": {
                            "type": "object",
                            "properties": {"inherits": role_array, "permissions": role_array},
                            "additionalProperties": false
                        }
                    },
                    "principals": {
                        "type": "object", "maxProperties": MAX_CONSTRAINTS,
                        "additionalProperties": {
                            "type": "object", "properties": {"roles": role_array}, "additionalProperties": false
                        }
                    },
                    "sessions": {
                        "type": "object", "maxProperties": MAX_CONSTRAINTS,
                        "additionalProperties": {
                            "type": "object",
                            "properties": {"principal": {"type": "string"}, "active_roles": role_array},
                            "required": ["principal"], "additionalProperties": false
                        }
                    }
                },
                "required": ["roles", "principals", "sessions"],
                "additionalProperties": false
            },
            "unknowns": {
                "type": "array", "maxItems": MAX_VARIABLES, "default": [],
                "items": {
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": {"kind": {"const": "principal_role"}, "principal": {"type": "string"}, "role": {"type": "string"}},
                            "required": ["kind", "principal", "role"], "additionalProperties": false
                        },
                        {
                            "type": "object",
                            "properties": {"kind": {"const": "session_role"}, "session": {"type": "string"}, "role": {"type": "string"}},
                            "required": ["kind", "session", "role"], "additionalProperties": false
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
