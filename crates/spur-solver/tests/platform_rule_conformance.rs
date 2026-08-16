use std::collections::BTreeSet;

use serde_json::{json, Value};
use spur_solver::{
    rules::{
        builtin_registry,
        execute::{prepare, run},
    },
    service::SolverService,
    types::SolveStatus,
};

struct ConformanceCase {
    rule_id: &'static str,
    valid: Value,
    invalid: Value,
}

fn design_case(
    rule_id: &'static str,
    rule: Value,
    valid_nodes: Value,
    invalid_nodes: Value,
) -> ConformanceCase {
    let request = |nodes| {
        json!({
            "family": "design",
            "mode": "verify",
            "rules": [rule],
            "scene": {"viewport": {"width": 390, "height": 844}, "nodes": nodes},
            "unknowns": []
        })
    };
    ConformanceCase {
        rule_id,
        valid: request(valid_nodes),
        invalid: request(invalid_nodes),
    }
}

fn accessibility_case(
    rule_id: &'static str,
    subjects: Value,
    parameters: Value,
    valid_elements: Value,
    invalid_elements: Value,
) -> ConformanceCase {
    let request = |elements| {
        json!({
            "family": "accessibility",
            "mode": "verify",
            "rules": [{"rule_id": rule_id, "subjects": subjects, "parameters": parameters}],
            "scene": {"viewport": {"width": 320, "height": 568}, "elements": elements},
            "unknowns": []
        })
    };
    ConformanceCase {
        rule_id,
        valid: request(valid_elements),
        invalid: request(invalid_elements),
    }
}

fn policy_case(
    rule_id: &'static str,
    subjects: Value,
    parameters: Value,
    valid_facts: Value,
    invalid_facts: Value,
) -> ConformanceCase {
    let request = |facts| {
        json!({
            "family": "policy",
            "mode": "verify",
            "rules": [{"rule_id": rule_id, "subjects": subjects, "parameters": parameters}],
            "facts": facts,
            "unknowns": []
        })
    };
    ConformanceCase {
        rule_id,
        valid: request(valid_facts),
        invalid: request(invalid_facts),
    }
}

fn resource_case(
    rule_id: &'static str,
    subjects: Value,
    parameters: Value,
    valid_facts: Value,
    invalid_facts: Value,
) -> ConformanceCase {
    let request = |facts| {
        json!({
            "family": "resource",
            "mode": "verify",
            "rules": [{"rule_id": rule_id, "subjects": subjects, "parameters": parameters}],
            "facts": facts,
            "unknowns": []
        })
    };
    ConformanceCase {
        rule_id,
        valid: request(valid_facts),
        invalid: request(invalid_facts),
    }
}

fn design_cases() -> Vec<ConformanceCase> {
    vec![
        design_case(
            "layout.axis_capacity",
            json!({
                "rule_id": "layout.axis_capacity",
                "subjects": ["container", "first", "second"],
                "parameters": {"axis": "horizontal", "gap": 20, "inset_start": 10, "inset_end": 10}
            }),
            json!({
                "container": {"rect": {"x": 0, "y": 0, "width": 100, "height": 1}},
                "first": {"rect": {"x": 0, "y": 0, "width": 30, "height": 1}},
                "second": {"rect": {"x": 0, "y": 0, "width": 30, "height": 1}}
            }),
            json!({
                "container": {"rect": {"x": 0, "y": 0, "width": 100, "height": 1}},
                "first": {"rect": {"x": 0, "y": 0, "width": 30, "height": 1}},
                "second": {"rect": {"x": 0, "y": 0, "width": 31, "height": 1}}
            }),
        ),
        design_case(
            "layout.containment",
            json!({"rule_id": "layout.containment", "subjects": ["child", "parent"]}),
            json!({
                "parent": {"rect": {"x": 0, "y": 0, "width": 100, "height": 100}},
                "child": {"rect": {"x": 76, "y": 0, "width": 24, "height": 24}}
            }),
            json!({
                "parent": {"rect": {"x": 0, "y": 0, "width": 100, "height": 100}},
                "child": {"rect": {"x": 77, "y": 0, "width": 24, "height": 24}}
            }),
        ),
        design_case(
            "layout.non_overlap",
            json!({"rule_id": "layout.non_overlap", "subjects": ["first", "second"], "parameters": {"minimum_gap": 24}}),
            json!({
                "first": {"rect": {"x": 0, "y": 0, "width": 24, "height": 24}},
                "second": {"rect": {"x": 48, "y": 0, "width": 24, "height": 24}}
            }),
            json!({
                "first": {"rect": {"x": 0, "y": 0, "width": 24, "height": 24}},
                "second": {"rect": {"x": 47, "y": 0, "width": 24, "height": 24}}
            }),
        ),
        design_case(
            "media.aspect_ratio",
            json!({"rule_id": "media.aspect_ratio", "subjects": ["media"], "parameters": {"source_width": 16, "source_height": 9}}),
            json!({"media": {"rect": {"x": 0, "y": 0, "width": 320, "height": 180}}}),
            json!({"media": {"rect": {"x": 0, "y": 0, "width": 320, "height": 181}}}),
        ),
    ]
}

fn accessibility_cases() -> Vec<ConformanceCase> {
    vec![
        accessibility_case(
            "a11y.target_size",
            json!(["target"]),
            json!({}),
            json!({"target": {"rect": {"x": 0, "y": 0, "width": 24, "height": 24}}}),
            json!({"target": {"rect": {"x": 0, "y": 0, "width": 23, "height": 24}}}),
        ),
        accessibility_case(
            "a11y.focus_not_obscured",
            json!(["focused", "obscurer"]),
            json!({}),
            json!({
                "focused": {"rect": {"x": 0, "y": 0, "width": 24, "height": 24}},
                "obscurer": {"rect": {"x": 12, "y": 0, "width": 24, "height": 24}}
            }),
            json!({
                "focused": {"rect": {"x": 0, "y": 0, "width": 24, "height": 24}},
                "obscurer": {"rect": {"x": 0, "y": 0, "width": 24, "height": 24}}
            }),
        ),
        accessibility_case(
            "a11y.reflow",
            json!(["content"]),
            json!({}),
            json!({"content": {"rect": {"x": 0, "y": 0, "width": 320, "height": 568}}}),
            json!({"content": {"rect": {"x": 0, "y": 0, "width": 321, "height": 568}}}),
        ),
        accessibility_case(
            "a11y.text_contrast",
            json!(["text"]),
            json!({}),
            json!({"text": {"foreground_luminance": 17500, "background_luminance": 0}}),
            json!({"text": {"foreground_luminance": 17499, "background_luminance": 0}}),
        ),
    ]
}

fn policy_cases() -> Vec<ConformanceCase> {
    let base_roles = json!({
        "admin": {"inherits": ["viewer"], "permissions": ["write"]},
        "viewer": {"inherits": [], "permissions": ["read"]},
        "auditor": {"inherits": [], "permissions": ["audit"]}
    });
    vec![
        policy_case(
            "rbac.permission_reachable",
            json!(["alice", "read"]),
            json!({}),
            json!({"roles": base_roles, "principals": {"alice": {"roles": ["admin"]}}, "sessions": {}}),
            json!({"roles": base_roles, "principals": {"alice": {"roles": []}}, "sessions": {}}),
        ),
        policy_case(
            "rbac.role_hierarchy_acyclic",
            json!([]),
            json!({}),
            json!({"roles": base_roles, "principals": {}, "sessions": {}}),
            json!({
                "roles": {
                    "admin": {"inherits": ["viewer"], "permissions": []},
                    "viewer": {"inherits": ["admin"], "permissions": []}
                },
                "principals": {}, "sessions": {}
            }),
        ),
        policy_case(
            "rbac.static_separation_of_duty",
            json!(["alice"]),
            json!({"roles": ["admin", "auditor"], "max_assigned": 1}),
            json!({"roles": base_roles, "principals": {"alice": {"roles": ["admin"]}}, "sessions": {}}),
            json!({"roles": base_roles, "principals": {"alice": {"roles": ["admin", "auditor"]}}, "sessions": {}}),
        ),
        policy_case(
            "rbac.dynamic_separation_of_duty",
            json!(["session"]),
            json!({"roles": ["admin", "auditor"], "max_active": 1}),
            json!({
                "roles": base_roles,
                "principals": {"alice": {"roles": ["admin", "auditor"]}},
                "sessions": {"session": {"principal": "alice", "active_roles": ["admin"]}}
            }),
            json!({
                "roles": base_roles,
                "principals": {"alice": {"roles": ["admin", "auditor"]}},
                "sessions": {"session": {"principal": "alice", "active_roles": ["admin", "auditor"]}}
            }),
        ),
    ]
}

fn resource_facts(replicas: i64, request: i64, domains: Value) -> Value {
    json!({
        "workloads": {"api": {
            "replicas": replicas,
            "requests": {"cpu": request},
            "limits": {"cpu": 500},
            "domain_counts": domains
        }},
        "pools": {"cluster": {"resources": {"cpu": 1500}}},
        "quotas": {"team": {"resources": {"cpu": 1500}}}
    })
}

fn resource_cases() -> Vec<ConformanceCase> {
    vec![
        resource_case(
            "resource.request_within_limit",
            json!(["api"]),
            json!({"resources": ["cpu"]}),
            resource_facts(1, 500, json!({"a": 1})),
            resource_facts(1, 501, json!({"a": 1})),
        ),
        resource_case(
            "resource.aggregate_capacity",
            json!(["cluster", "api"]),
            json!({"resources": ["cpu"]}),
            resource_facts(3, 500, json!({"a": 2, "b": 1})),
            resource_facts(4, 500, json!({"a": 2, "b": 2})),
        ),
        resource_case(
            "resource.quota_capacity",
            json!(["team", "api"]),
            json!({"resources": ["cpu"]}),
            resource_facts(3, 500, json!({"a": 2, "b": 1})),
            resource_facts(4, 500, json!({"a": 2, "b": 2})),
        ),
        resource_case(
            "placement.topology_max_skew",
            json!(["api"]),
            json!({"max_skew": 1}),
            resource_facts(3, 500, json!({"a": 2, "b": 1})),
            resource_facts(4, 500, json!({"a": 3, "b": 1})),
        ),
        resource_case(
            "placement.minimum_failure_domains",
            json!(["api"]),
            json!({"minimum_domains": 2}),
            resource_facts(2, 500, json!({"a": 1, "b": 1})),
            resource_facts(1, 500, json!({"a": 1, "b": 0})),
        ),
    ]
}

fn cases() -> Vec<ConformanceCase> {
    design_cases()
        .into_iter()
        .chain(accessibility_cases())
        .chain(policy_cases())
        .chain(resource_cases())
        .collect()
}

#[tokio::test]
async fn every_implemented_hard_rule_accepts_a_boundary_and_rejects_a_counterexample() {
    let cases = cases();
    let tested = cases
        .iter()
        .map(|case| case.rule_id)
        .collect::<BTreeSet<_>>();
    let registry = serde_json::to_value(builtin_registry()).expect("serialize rule registry");
    let implemented = registry["rules"]
        .as_array()
        .expect("registry rules")
        .iter()
        .filter(|rule| rule["availability"] == "implemented" && rule["default_strength"] == "hard")
        .map(|rule| rule["id"].as_str().expect("rule id"))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        tested, implemented,
        "conformance cases must cover the catalog"
    );

    let service = SolverService::new();
    for case in cases {
        let valid = run(
            &service,
            prepare(case.valid).unwrap_or_else(|error| {
                panic!("{} valid fixture must compile: {error}", case.rule_id)
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("{} valid fixture must run: {error}", case.rule_id));
        assert_eq!(
            valid.solver.status,
            SolveStatus::Sat,
            "{} boundary",
            case.rule_id
        );

        let invalid = run(
            &service,
            prepare(case.invalid).unwrap_or_else(|error| {
                panic!("{} invalid fixture must compile: {error}", case.rule_id)
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("{} invalid fixture must run: {error}", case.rule_id));
        assert_eq!(
            invalid.solver.status,
            SolveStatus::Unsat,
            "{} counterexample",
            case.rule_id
        );
        assert_eq!(
            invalid.rule_results.len(),
            1,
            "{} attribution",
            case.rule_id
        );
        assert_eq!(invalid.rule_results[0].rule_id, case.rule_id);
        assert_eq!(invalid.rule_results[0].status, SolveStatus::Unsat);
    }
}
