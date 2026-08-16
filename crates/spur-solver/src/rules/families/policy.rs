//! Finite RBAC rules over caller-supplied roles, principals, and sessions.

pub mod compile;

use std::sync::LazyLock;

use serde_json::json;

use crate::rules::catalog::{
    LlmEncoding, RuleAuthority, RuleDefinition, RuleExample, RuleExamples, RuleFamily,
    RuleGuidance, RuleProfile, RuleRegistry, RuleStrength, SolverEncoding,
};

pub use compile::COMPILER;

static BUILTIN_REGISTRY: LazyLock<RuleRegistry> = LazyLock::new(|| {
    RuleRegistry::new(
        1,
        vec![RuleFamily::new(
            "policy",
            "Finite RBAC reachability, hierarchy, and separation-of-duty rules.",
            ["nist_rbac"],
        )],
        vec![RuleProfile::new(
            "nist_rbac",
            "policy",
            "Core, hierarchical, static-separation, and dynamic-separation RBAC constraints.",
            [
                "rbac.dynamic_separation_of_duty",
                "rbac.minimum_privilege",
                "rbac.permission_reachable",
                "rbac.role_hierarchy_acyclic",
                "rbac.static_separation_of_duty",
            ],
        )],
        vec![
            dynamic_separation_rule(),
            minimum_privilege_rule(),
            permission_reachable_rule(),
            role_hierarchy_acyclic_rule(),
            static_separation_rule(),
        ],
    )
    .unwrap_or_else(|error| panic!("built-in policy registry is invalid: {error}"))
});

/// Returns the validated policy catalog contribution.
#[must_use]
pub fn builtin_registry() -> &'static RuleRegistry {
    &BUILTIN_REGISTRY
}

fn permission_reachable_rule() -> RuleDefinition {
    rule(
        "rbac.permission_reachable",
        "permission_reachability",
        "Require a principal to hold a role that reaches the requested permission.",
        ["principal.roles", "roles.inherits", "roles.permissions"],
        ["OR assigned(principal, role) for every role whose closure grants permission"],
    )
}

fn role_hierarchy_acyclic_rule() -> RuleDefinition {
    rule(
        "rbac.role_hierarchy_acyclic",
        "strict_role_rank",
        "Require every inheritance edge to admit a strict bounded rank order.",
        ["roles.inherits"],
        ["rank(inherited_role) < rank(role) for every inheritance edge"],
    )
}

fn static_separation_rule() -> RuleDefinition {
    rule(
        "rbac.static_separation_of_duty",
        "assigned_role_cardinality",
        "Limit mutually exclusive role assignments for one principal.",
        [
            "principal.roles",
            "parameters.roles",
            "parameters.max_assigned",
        ],
        ["sum assigned(principal, selected_role) <= max_assigned"],
    )
}

fn dynamic_separation_rule() -> RuleDefinition {
    rule(
        "rbac.dynamic_separation_of_duty",
        "active_role_cardinality",
        "Limit mutually exclusive active roles in one session.",
        [
            "session.active_roles",
            "parameters.roles",
            "parameters.max_active",
        ],
        ["sum active(session, selected_role) <= max_active"],
    )
}

fn minimum_privilege_rule() -> RuleDefinition {
    RuleDefinition::new(
        "rbac.minimum_privilege",
        "policy",
        "nist_rbac",
        "minimum_privilege",
        "Prefer fewer grants while preserving explicitly declared required permissions.",
    )
    .with_guidance(RuleGuidance::capability_unavailable(
        "minimum privilege requires caller-owned utility requirements and an optimization objective",
        RuleStrength::Advisory,
        vec![nist_authority()],
        ["required_permissions", "grant_costs"],
        LlmEncoding::new(
            "conditional",
            ["rank feasible policies after requirements are explicit"],
            ["Declare required permissions", "Declare grant costs", "Use an optimization objective"],
            ["Do not infer business-required permissions"],
            ["Escalate until utility requirements are explicit"],
        ),
    ))
}

fn rule(
    id: &str,
    primitive: &str,
    summary: &str,
    requires: impl IntoIterator<Item = &'static str>,
    formula: impl IntoIterator<Item = &'static str>,
) -> RuleDefinition {
    RuleDefinition::new(id, "policy", "nist_rbac", primitive, summary).with_guidance(
        RuleGuidance::implemented_hard(
            vec![nist_authority()],
            requires,
            LlmEncoding::new(
                "high",
                [summary],
                [
                    "Validate finite RBAC facts",
                    "Normalize graph closure",
                    "Compile membership and rank predicates",
                ],
                ["Do not infer assignments from an external IAM service"],
                ["Escalate attribute-based or conditional policies to a richer policy family"],
            ),
            SolverEncoding::new(
                "QF_LIA",
                "assert the RBAC predicate over complete memberships",
                "leave only explicitly declared membership unknowns free",
                formula,
            ),
            RuleExamples::new(
                RuleExample::new(json!({"result": "boundary"}), "pass", None::<String>),
                RuleExample::new(
                    json!({"result": "violation"}),
                    "counterexample",
                    Some(format!("{id}.violation")),
                ),
            ),
        ),
    )
}

fn nist_authority() -> RuleAuthority {
    RuleAuthority::new(
        "nist_standard_model",
        "NIST Role Based Access Control",
        "https://csrc.nist.gov/Projects/role-based-access-control/faqs",
    )
}
