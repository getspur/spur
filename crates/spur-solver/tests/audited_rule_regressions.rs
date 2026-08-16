use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use serde_json::{json, Value};
use spur_solver::{
    process::{ProcessFuture, ProcessOutcome, ProcessOutput, ProcessRequest, ProcessRunner},
    rules::{
        builtin_registry,
        execute::{prepare, run},
    },
    service::SolverService,
    types::{ModelValue, SolveStatus},
};

fn compilation_error(request: Value) -> String {
    prepare(request)
        .expect_err("malformed family request must be rejected")
        .to_string()
}

#[test]
fn capacity_rejects_selected_resource_absent_from_capacity() {
    let error = compilation_error(json!({
        "family": "resource",
        "mode": "verify",
        "rules": [{
            "rule_id": "resource.aggregate_capacity",
            "subjects": ["cluster", "api"],
            "parameters": {"resources": ["memory"]}
        }],
        "facts": {
            "workloads": {
                "api": {
                    "replicas": 1,
                    "requests": {"memory": 128},
                    "limits": {"memory": 256},
                    "domain_counts": {}
                }
            },
            "pools": {"cluster": {"resources": {"cpu": 1000}}},
            "quotas": {}
        },
        "unknowns": []
    }));

    assert!(error.contains("pool `cluster` does not declare resource `memory`"));
}

#[test]
fn quota_rejects_selected_resource_absent_from_capacity() {
    let error = compilation_error(json!({
        "family": "resource",
        "mode": "verify",
        "rules": [{
            "rule_id": "resource.quota_capacity",
            "subjects": ["team-a", "api"],
            "parameters": {"resources": ["memory"]}
        }],
        "facts": {
            "workloads": {
                "api": {
                    "replicas": 1,
                    "requests": {"memory": 128},
                    "limits": {"memory": 256},
                    "domain_counts": {}
                }
            },
            "pools": {},
            "quotas": {"team-a": {"resources": {"cpu": 1000}}}
        },
        "unknowns": []
    }));

    assert!(error.contains("quota `team-a` does not declare resource `memory`"));
}

#[test]
fn accessibility_synthesis_rejects_unknown_over_concrete_field() {
    let error = compilation_error(json!({
        "family": "accessibility",
        "mode": "synthesize",
        "rules": [{"rule_id": "a11y.target_size", "subjects": ["save"]}],
        "scene": {
            "viewport": {"width": 320, "height": 568},
            "elements": {
                "save": {"rect": {"x": 0, "y": 0, "width": 24, "height": 24}}
            }
        },
        "unknowns": [{"subject": "save", "field": "width", "min": 1, "max": 1}]
    }));

    assert!(error.contains("save.width is already fixed"));
}

#[test]
fn policy_rejects_unauthorized_fixed_session_role() {
    let error = compilation_error(json!({
        "family": "policy",
        "mode": "verify",
        "rules": [{
            "rule_id": "rbac.dynamic_separation_of_duty",
            "subjects": ["alice-session"],
            "parameters": {"roles": ["admin", "auditor"], "max_active": 1}
        }],
        "facts": {
            "roles": {
                "admin": {"inherits": [], "permissions": ["write"]},
                "auditor": {"inherits": [], "permissions": ["audit"]}
            },
            "principals": {"alice": {"roles": ["admin"]}},
            "sessions": {
                "alice-session": {"principal": "alice", "active_roles": ["auditor"]}
            }
        },
        "unknowns": []
    }));

    assert!(error.contains("session `alice-session` activates unauthorized role `auditor`"));
}

#[test]
fn policy_accepts_active_role_inherited_transitively_by_assigned_role() {
    prepare(json!({
        "family": "policy",
        "mode": "verify",
        "rules": [{
            "rule_id": "rbac.dynamic_separation_of_duty",
            "subjects": ["alice-session"],
            "parameters": {"roles": ["viewer", "auditor"], "max_active": 1}
        }],
        "facts": {
            "roles": {
                "admin": {"inherits": ["editor"], "permissions": ["write"]},
                "editor": {"inherits": ["viewer"], "permissions": ["edit"]},
                "viewer": {"inherits": [], "permissions": ["read"]},
                "auditor": {"inherits": [], "permissions": ["audit"]}
            },
            "principals": {"alice": {"roles": ["admin"]}},
            "sessions": {
                "alice-session": {"principal": "alice", "active_roles": ["viewer"]}
            }
        },
        "unknowns": []
    }))
    .expect("inherited role is authorized for activation");
}

#[test]
fn policy_rejects_activation_in_reverse_inheritance_direction() {
    let error = compilation_error(json!({
        "family": "policy",
        "mode": "verify",
        "rules": [{
            "rule_id": "rbac.dynamic_separation_of_duty",
            "subjects": ["alice-session"],
            "parameters": {"roles": ["admin"], "max_active": 1}
        }],
        "facts": {
            "roles": {
                "admin": {"inherits": ["viewer"], "permissions": ["write"]},
                "viewer": {"inherits": [], "permissions": ["read"]}
            },
            "principals": {"alice": {"roles": ["viewer"]}},
            "sessions": {
                "alice-session": {"principal": "alice", "active_roles": ["admin"]}
            }
        },
        "unknowns": []
    }));

    assert!(error.contains("session `alice-session` activates unauthorized role `admin`"));
}

#[tokio::test]
async fn policy_synthesis_authorizes_fixed_session_role() {
    let prepared = prepare(json!({
        "family": "policy",
        "mode": "synthesize",
        "rules": [{
            "rule_id": "rbac.dynamic_separation_of_duty",
            "subjects": ["alice-session"],
            "parameters": {"roles": ["auditor"], "max_active": 1}
        }],
        "facts": {
            "roles": {
                "admin": {"inherits": ["editor"], "permissions": ["write"]},
                "editor": {"inherits": ["auditor"], "permissions": ["edit"]},
                "auditor": {"inherits": [], "permissions": ["audit"]}
            },
            "principals": {"alice": {"roles": []}},
            "sessions": {
                "alice-session": {"principal": "alice", "active_roles": ["auditor"]}
            }
        },
        "unknowns": [{"kind": "principal_role", "principal": "alice", "role": "admin"}]
    }))
    .expect("unknown principal assignment can authorize a fixed active role");

    let response = run(&SolverService::new(), prepared)
        .await
        .expect("policy synthesis response");
    assert_eq!(response.solver.status, SolveStatus::Sat);
    assert_eq!(response.assignments[0].value, ModelValue::Int(1));
}

#[test]
fn policy_authorization_constraints_scale_with_unknown_memberships() {
    let roles = (0..32)
        .map(|index| {
            (
                format!("role-{index}"),
                json!({
                    "inherits": [],
                    "permissions": if index == 0 { json!(["read"]) } else { json!([]) }
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let sessions = (0..32)
        .map(|index| {
            (
                format!("session-{index}"),
                json!({"principal": "alice", "active_roles": []}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let prepared = prepare(json!({
        "family": "policy",
        "mode": "verify",
        "rules": [{"rule_id": "rbac.permission_reachable", "subjects": ["alice", "read"]}],
        "facts": {
            "roles": roles,
            "principals": {"alice": {"roles": ["role-0"]}},
            "sessions": sessions
        },
        "unknowns": []
    }))
    .expect("fixed-false session memberships need no authorization predicates");

    let encoded = serde_json::to_vec(&prepared.request.constraints[0])
        .expect("serialize compiled policy constraint");
    assert!(
        encoded.len() < 10_000,
        "constraint grew to {} bytes",
        encoded.len()
    );
}

#[test]
fn policy_authorization_constraints_remain_bounded_at_unknown_limit() {
    let roles = (0..32)
        .map(|index| {
            (
                format!("role-{index}"),
                json!({
                    "inherits": [],
                    "permissions": if index == 0 { json!(["read"]) } else { json!([]) }
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let sessions = (0..32)
        .map(|index| {
            (
                format!("session-{index}"),
                json!({"principal": "alice", "active_roles": []}),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let unknowns = (0..32)
        .map(|index| {
            json!({
                "kind": "principal_role",
                "principal": "alice",
                "role": format!("role-{index}")
            })
        })
        .chain((0..32).map(|index| {
            json!({
                "kind": "session_role",
                "session": format!("session-{index}"),
                "role": format!("role-{index}")
            })
        }))
        .collect::<Vec<_>>();
    let prepared = prepare(json!({
        "family": "policy",
        "mode": "synthesize",
        "rules": [{"rule_id": "rbac.permission_reachable", "subjects": ["alice", "read"]}],
        "facts": {
            "roles": roles,
            "principals": {"alice": {"roles": []}},
            "sessions": sessions
        },
        "unknowns": unknowns
    }))
    .expect("family-limit unknown memberships compile");

    let encoded = serde_json::to_vec(&prepared.request.constraints[0])
        .expect("serialize compiled policy constraint");
    assert!(
        encoded.len() < 50_000,
        "constraint grew to {} bytes",
        encoded.len()
    );
}

fn placement_request(rule: Value, replicas: i64, domain_counts: Value) -> Value {
    json!({
        "family": "resource",
        "mode": "verify",
        "rules": [rule],
        "facts": {
            "workloads": {
                "api": {
                    "replicas": replicas,
                    "requests": {},
                    "limits": {},
                    "domain_counts": domain_counts
                }
            },
            "pools": {},
            "quotas": {}
        },
        "unknowns": [],
        "timeout_ms": 30000
    })
}

#[tokio::test]
async fn placement_rejects_domain_counts_that_do_not_conserve_replicas() {
    let prepared = prepare(placement_request(
        json!({
            "rule_id": "placement.topology_max_skew",
            "subjects": ["api"],
            "parameters": {"max_skew": 1}
        }),
        1,
        json!({"zone-a": 100, "zone-b": 100}),
    ))
    .expect("well-shaped placement request");

    let response = run(&SolverService::new(), prepared)
        .await
        .expect("solver response");
    assert_eq!(response.solver.status, SolveStatus::Unsat);
    assert_eq!(response.rule_results[0].status, SolveStatus::Unsat);
}

#[test]
fn topology_max_skew_must_be_positive() {
    let error = compilation_error(placement_request(
        json!({
            "rule_id": "placement.topology_max_skew",
            "subjects": ["api"],
            "parameters": {"max_skew": 0}
        }),
        2,
        json!({"zone-a": 1, "zone-b": 1}),
    ));

    assert!(error.contains("`max_skew` must be positive"));
}

fn reflow_request(x: i64, width: i64) -> Value {
    json!({
        "family": "accessibility",
        "mode": "verify",
        "rules": [{"rule_id": "a11y.reflow", "subjects": ["content"]}],
        "scene": {
            "viewport": {"width": 320, "height": 568},
            "elements": {
                "content": {"rect": {"x": x, "y": 0, "width": width, "height": 568}}
            }
        },
        "unknowns": [],
        "timeout_ms": 30000
    })
}

#[tokio::test]
async fn reflow_rejects_horizontal_extent_past_viewport() {
    let response = run(
        &SolverService::new(),
        prepare(reflow_request(1, 320)).expect("well-shaped reflow request"),
    )
    .await
    .expect("solver response");

    assert_eq!(response.solver.status, SolveStatus::Unsat);
}

#[tokio::test]
async fn reflow_rejects_left_overflow() {
    let response = run(
        &SolverService::new(),
        prepare(reflow_request(-1, 320)).expect("well-shaped reflow request"),
    )
    .await
    .expect("solver response");

    assert_eq!(response.solver.status, SolveStatus::Unsat);
}

#[test]
fn reflow_rejects_target_size_exception_kinds() {
    for kind in ["spacing", "essential"] {
        let mut request = reflow_request(0, 400);
        request["rules"][0]["parameters"] = json!({
            "exception": {"kind": kind, "evidence": "reviewed"}
        });
        let error = compilation_error(request);
        assert!(
            error.contains("rule `a11y.reflow` does not accept this exception kind"),
            "unexpected {kind} diagnostic: {error}"
        );
    }
}

#[tokio::test]
async fn reflow_accepts_two_dimensional_exception() {
    let mut request = reflow_request(-10, 400);
    request["rules"][0]["parameters"] = json!({
        "exception": {"kind": "two_dimensional", "evidence": "data-grid"}
    });
    let response = run(
        &SolverService::new(),
        prepare(request).expect("two-dimensional reflow exception compiles"),
    )
    .await
    .expect("solver response");

    assert_eq!(response.solver.status, SolveStatus::Sat);
}

#[tokio::test]
async fn target_size_accepts_spacing_exception_with_evidence() {
    let prepared = prepare(json!({
        "family": "accessibility",
        "mode": "verify",
        "rules": [{
            "rule_id": "a11y.target_size",
            "subjects": ["isolated-control"],
            "parameters": {
                "exception": {"kind": "spacing", "evidence": "layout-audit:isolated-control"}
            }
        }],
        "scene": {
            "viewport": {"width": 320, "height": 568},
            "elements": {
                "isolated-control": {"rect": {"x": 0, "y": 0, "width": 20, "height": 20}}
            }
        },
        "unknowns": []
    }))
    .expect("WCAG spacing is a typed target-size exception");

    let response = run(&SolverService::new(), prepared)
        .await
        .expect("solver response");
    assert_eq!(response.solver.status, SolveStatus::Sat);
}

#[tokio::test]
async fn policy_and_resource_catalog_examples_are_executable() {
    let service = SolverService::new();
    for rule in builtin_registry().rules().iter().filter(|rule| {
        matches!(rule.family(), "policy" | "resource") && rule.id() != "rbac.minimum_privilege"
    }) {
        let catalog = serde_json::to_value(rule).expect("serialize catalog rule");
        for (example, expected) in [
            (&catalog["examples"]["valid"], SolveStatus::Sat),
            (&catalog["examples"]["invalid"], SolveStatus::Unsat),
        ] {
            let request = example["facts"].clone();
            let prepared = prepare(request).unwrap_or_else(|error| {
                panic!("{} catalog example must compile: {error}", rule.id())
            });
            let response = run(&service, prepared).await.unwrap_or_else(|error| {
                panic!("{} catalog example must solve: {error}", rule.id())
            });
            assert_eq!(
                response.solver.status,
                expected,
                "{} {} example",
                rule.id(),
                example["expectation"]
            );
        }
    }
}

#[derive(Debug)]
struct DelayedUnsatRunner {
    calls: AtomicUsize,
    delay: Duration,
}

impl ProcessRunner for DelayedUnsatRunner {
    fn run(&self, request: ProcessRequest) -> ProcessFuture<'_> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let completion = tokio::time::Instant::now() + self.delay;
            if completion >= request.deadline() {
                tokio::time::sleep_until(request.deadline()).await;
                return Ok(ProcessOutcome::TimedOut);
            }
            tokio::time::sleep_until(completion).await;
            Ok(ProcessOutcome::Completed(ProcessOutput {
                success: true,
                exit_code: Some(0),
                stdout: b"unsat\n".to_vec(),
                stderr: Vec::new(),
            }))
        })
    }
}

#[derive(Debug, Default)]
struct RecordingUnsatRunner {
    deadlines: Mutex<Vec<tokio::time::Instant>>,
}

impl ProcessRunner for RecordingUnsatRunner {
    fn run(&self, request: ProcessRequest) -> ProcessFuture<'_> {
        Box::pin(async move {
            self.deadlines
                .lock()
                .expect("deadline recorder lock")
                .push(request.deadline());
            Ok(ProcessOutcome::Completed(ProcessOutput {
                success: true,
                exit_code: Some(0),
                stdout: b"unsat\n".to_vec(),
                stderr: Vec::new(),
            }))
        })
    }
}

#[tokio::test]
async fn attribution_deadlines_never_exceed_aggregate_deadline() {
    let runner = Arc::new(RecordingUnsatRunner::default());
    let service_runner: Arc<dyn ProcessRunner> = Arc::<RecordingUnsatRunner>::clone(&runner);
    let service = SolverService::with_runner(service_runner);
    let prepared = prepare(json!({
        "family": "design",
        "mode": "verify",
        "rules": [
            {"rule_id": "layout.containment", "subjects": ["first", "container"]},
            {"rule_id": "layout.non_overlap", "subjects": ["first", "second"]}
        ],
        "scene": {
            "viewport": {"width": 320, "height": 568},
            "nodes": {
                "container": {"rect": {"x": 0, "y": 0, "width": 320, "height": 100}},
                "first": {"rect": {"x": 0, "y": 0, "width": 24, "height": 24}},
                "second": {"rect": {"x": 48, "y": 0, "width": 24, "height": 24}}
            }
        },
        "unknowns": [],
        "timeout_ms": 120
    }))
    .expect("deadline fixture compiles");

    let response = run(&service, prepared).await.expect("attribution solve");
    assert_eq!(response.rule_results.len(), 2);

    let deadlines = runner.deadlines.lock().expect("deadline recorder lock");
    assert_eq!(deadlines.len(), 3, "aggregate plus two attribution calls");
    let aggregate_deadline = deadlines[0];
    assert!(deadlines[1..]
        .iter()
        .all(|deadline| *deadline <= aggregate_deadline));
}

#[tokio::test]
async fn failure_attribution_shares_the_aggregate_deadline() {
    let runner = Arc::new(DelayedUnsatRunner {
        calls: AtomicUsize::new(0),
        delay: Duration::from_millis(70),
    });
    let service_runner: Arc<dyn ProcessRunner> = Arc::<DelayedUnsatRunner>::clone(&runner);
    let service = SolverService::with_runner(service_runner);
    let prepared = prepare(json!({
        "family": "design",
        "mode": "verify",
        "rules": [
            {
                "rule_id": "layout.axis_capacity",
                "subjects": ["container", "first", "second"],
                "parameters": {"axis": "horizontal"}
            },
            {"rule_id": "layout.containment", "subjects": ["first", "container"]},
            {"rule_id": "layout.non_overlap", "subjects": ["first", "second"]},
            {
                "rule_id": "media.aspect_ratio",
                "subjects": ["media"],
                "parameters": {"source_width": 16, "source_height": 9}
            }
        ],
        "scene": {
            "viewport": {"width": 320, "height": 568},
            "nodes": {
                "container": {"rect": {"x": 0, "y": 0, "width": 320, "height": 100}},
                "first": {"rect": {"x": 0, "y": 0, "width": 24, "height": 24}},
                "second": {"rect": {"x": 48, "y": 0, "width": 24, "height": 24}},
                "media": {"rect": {"x": 0, "y": 100, "width": 320, "height": 180}}
            }
        },
        "unknowns": [],
        "timeout_ms": 120
    }))
    .expect("attribution fixture compiles");

    let started = tokio::time::Instant::now();
    let response = run(&service, prepared).await.expect("attribution solve");
    let elapsed = started.elapsed();

    assert!(
        runner.calls.load(Ordering::SeqCst) <= 2,
        "aggregate plus at most one attribution solve may reach the runner"
    );
    assert!(elapsed < Duration::from_millis(240));
    assert!(response.total_duration_ms > response.solver.duration_ms);
    assert_eq!(response.rule_results.len(), 4);
    assert!(response
        .rule_results
        .iter()
        .all(|result| result.status == SolveStatus::Timeout));
}
