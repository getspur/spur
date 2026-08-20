use std::collections::BTreeSet;

use spur_solver::{
    rules::{
        execute::{prepare, run},
        manifest::{
            manifest_conformance_vectors, manifest_executable_rule_ids, manifest_rule_handler,
        },
        manifest_format::{ConformanceVectorV1, NativeHandlerV1},
    },
    service::SolverService,
    types::SolveStatus,
};

struct ConformanceCase<'a> {
    rule_id: &'a str,
    handler: NativeHandlerV1,
    valid: &'a [ConformanceVectorV1],
    invalid: &'a [ConformanceVectorV1],
}

fn cases() -> Vec<ConformanceCase<'static>> {
    manifest_executable_rule_ids()
        .iter()
        .map(|rule_id| {
            let handler = manifest_rule_handler(rule_id)
                .unwrap_or_else(|| panic!("executable rule `{rule_id}` has no native handler"));
            let vectors = manifest_conformance_vectors(rule_id).unwrap_or_else(|| {
                panic!("executable rule `{rule_id}` has no conformance vectors")
            });

            ConformanceCase {
                rule_id,
                handler,
                valid: &vectors.valid,
                invalid: &vectors.invalid,
            }
        })
        .collect()
}

#[test]
fn manifest_vectors_cover_every_executable_rule_and_native_handler_exactly_once() {
    let cases = cases();
    let expected_rule_ids = manifest_executable_rule_ids()
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut tested_rule_ids = BTreeSet::new();
    let mut tested_handlers = BTreeSet::new();

    assert_eq!(
        cases.len(),
        expected_rule_ids.len(),
        "conformance must contain one vector set per executable rule"
    );
    for case in &cases {
        assert!(
            tested_rule_ids.insert(case.rule_id),
            "duplicate conformance vector set for `{}`",
            case.rule_id
        );
        assert!(
            tested_handlers.insert(case.handler),
            "native handler `{:?}` is shared by multiple conformance rule sets",
            case.handler
        );
        assert!(
            !case.valid.is_empty(),
            "`{}` must declare at least one valid conformance vector",
            case.rule_id
        );
        assert!(
            !case.invalid.is_empty(),
            "`{}` must declare at least one invalid conformance vector",
            case.rule_id
        );
    }

    assert_eq!(
        tested_rule_ids, expected_rule_ids,
        "conformance rule IDs must exactly match the executable manifest registry"
    );
    assert_eq!(
        tested_handlers,
        NativeHandlerV1::ALL.iter().copied().collect(),
        "conformance must cover every closed native handler exactly once"
    );
}

#[tokio::test]
async fn every_manifest_valid_vector_passes_and_invalid_vector_is_rejected() {
    let service = SolverService::new();

    for case in cases() {
        for vector in case.valid {
            let prepared = prepare(vector.request.clone()).unwrap_or_else(|error| {
                panic!(
                    "{} valid vector `{}` must compile: {error}",
                    case.rule_id, vector.name
                )
            });
            let result = run(&service, prepared).await.unwrap_or_else(|error| {
                panic!(
                    "{} valid vector `{}` must run: {error}",
                    case.rule_id, vector.name
                )
            });
            assert_eq!(
                result.solver.status,
                SolveStatus::Sat,
                "{} valid vector `{}`",
                case.rule_id,
                vector.name
            );
        }

        for vector in case.invalid {
            let Ok(prepared) = prepare(vector.request.clone()) else {
                continue;
            };
            let Ok(result) = run(&service, prepared).await else {
                continue;
            };

            assert_ne!(
                result.solver.status,
                SolveStatus::Sat,
                "{} invalid vector `{}` was accepted",
                case.rule_id,
                vector.name
            );
            if result.solver.status == SolveStatus::Unsat {
                assert!(
                    result.rule_results.iter().any(|rule| {
                        rule.rule_id == case.rule_id && rule.status == SolveStatus::Unsat
                    }),
                    "{} invalid vector `{}` lacked per-rule rejection attribution",
                    case.rule_id,
                    vector.name
                );
            }
        }
    }
}
