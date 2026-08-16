//! Resource capacity and placement rules over caller-supplied platform facts.

pub mod compile;

use std::sync::LazyLock;

use serde_json::json;

use crate::rules::catalog::{
    LlmEncoding, RuleAuthority, RuleDefinition, RuleExample, RuleExamples, RuleFamily,
    RuleGuidance, RuleProfile, RuleRegistry, SolverEncoding,
};

pub use compile::COMPILER;

static BUILTIN_REGISTRY: LazyLock<RuleRegistry> = LazyLock::new(|| {
    RuleRegistry::new(
        1,
        vec![RuleFamily::new(
            "resource",
            "Resource demand, capacity, quota, and topology placement constraints.",
            ["capacity", "topology_placement"],
        )],
        vec![
            RuleProfile::new(
                "capacity",
                "resource",
                "Per-workload and aggregate resource capacity constraints.",
                [
                    "resource.aggregate_capacity",
                    "resource.quota_capacity",
                    "resource.request_within_limit",
                ],
            ),
            RuleProfile::new(
                "topology_placement",
                "resource",
                "Finite failure-domain count and skew constraints.",
                [
                    "placement.minimum_failure_domains",
                    "placement.topology_max_skew",
                ],
            ),
        ],
        vec![
            aggregate_capacity_rule(),
            minimum_failure_domains_rule(),
            quota_capacity_rule(),
            request_within_limit_rule(),
            topology_max_skew_rule(),
        ],
    )
    .unwrap_or_else(|error| panic!("built-in resource registry is invalid: {error}"))
});

/// Returns the validated resource catalog contribution.
#[must_use]
pub fn builtin_registry() -> &'static RuleRegistry {
    &BUILTIN_REGISTRY
}

fn request_within_limit_rule() -> RuleDefinition {
    capacity_rule(
        "resource.request_within_limit",
        "request_limit",
        "Require each selected per-replica request to be at most its matching limit.",
        ["workload.requests", "workload.limits"],
        ["request(resource) <= limit(resource)"],
    )
}

fn aggregate_capacity_rule() -> RuleDefinition {
    capacity_rule(
        "resource.aggregate_capacity",
        "aggregate_capacity",
        "Require aggregate replica demand to fit each selected pool capacity.",
        ["workloads.replicas", "workloads.requests", "pool.resources"],
        ["sum workload.replicas * workload.request(resource) <= pool.capacity(resource)"],
    )
}

fn quota_capacity_rule() -> RuleDefinition {
    capacity_rule(
        "resource.quota_capacity",
        "quota_capacity",
        "Require aggregate replica demand to fit each selected quota capacity.",
        [
            "workloads.replicas",
            "workloads.requests",
            "quota.resources",
        ],
        ["sum workload.replicas * workload.request(resource) <= quota.capacity(resource)"],
    )
}

fn topology_max_skew_rule() -> RuleDefinition {
    placement_rule(
        "placement.topology_max_skew",
        "pairwise_max_skew",
        "Limit the pairwise difference between declared topology-domain counts.",
        ["workload.domain_counts", "parameters.max_skew"],
        [
            "count(a) - count(b) <= max_skew",
            "count(b) - count(a) <= max_skew",
        ],
    )
}

fn minimum_failure_domains_rule() -> RuleDefinition {
    placement_rule(
        "placement.minimum_failure_domains",
        "positive_domain_cardinality",
        "Require positive placement counts in at least the minimum number of declared domains.",
        ["workload.domain_counts", "parameters.minimum_domains"],
        [
            "sum present(domain) >= minimum_domains",
            "present(domain) iff count(domain) > 0",
        ],
    )
}

fn capacity_rule(
    id: &str,
    primitive: &str,
    summary: &str,
    requires: impl IntoIterator<Item = &'static str>,
    formula: impl IntoIterator<Item = &'static str>,
) -> RuleDefinition {
    rule(
        id,
        "capacity",
        primitive,
        summary,
        requires,
        formula,
        resource_authority(),
    )
}

fn placement_rule(
    id: &str,
    primitive: &str,
    summary: &str,
    requires: impl IntoIterator<Item = &'static str>,
    formula: impl IntoIterator<Item = &'static str>,
) -> RuleDefinition {
    rule(
        id,
        "topology_placement",
        primitive,
        summary,
        requires,
        formula,
        topology_authority(),
    )
}

fn rule(
    id: &str,
    profile: &str,
    primitive: &str,
    summary: &str,
    requires: impl IntoIterator<Item = &'static str>,
    formula: impl IntoIterator<Item = &'static str>,
    authority: RuleAuthority,
) -> RuleDefinition {
    RuleDefinition::new(id, "resource", profile, primitive, summary).with_guidance(
        RuleGuidance::implemented_hard(
            vec![authority],
            requires,
            LlmEncoding::new(
                "high",
                [summary],
                [
                    "Bind explicit workloads and resources",
                    "Declare bounded unknown numeric facts",
                    "Compile capacity or placement predicates",
                ],
                ["Do not query or infer live scheduler state"],
                ["Escalate affinity, taints, or priority to separate rule primitives"],
            ),
            SolverEncoding::new(
                "QF_NIA",
                "assert the predicate over complete demand and placement facts",
                "leave only explicitly bounded numeric facts free",
                formula,
            ),
            RuleExamples::new(
                RuleExample::new(json!({"result": "boundary"}), "pass", None::<String>),
                RuleExample::new(
                    json!({"result": "one_unit_over"}),
                    "counterexample",
                    Some(format!("{id}.violation")),
                ),
            ),
        ),
    )
}

fn resource_authority() -> RuleAuthority {
    RuleAuthority::new(
        "kubernetes_documentation",
        "Resource Management for Pods and Containers",
        "https://kubernetes.io/docs/concepts/configuration/manage-resources-containers/",
    )
}

fn topology_authority() -> RuleAuthority {
    RuleAuthority::new(
        "kubernetes_documentation",
        "Pod Topology Spread Constraints",
        "https://kubernetes.io/docs/concepts/scheduling-eviction/topology-spread-constraints/",
    )
}
