use std::sync::Arc;

use serde_json::{json, Map, Value};
use spur_mcp::{ServerKind, ToolAuthority, ToolCallContext, ToolRegistry};
use spur_solver::{
    mcp::SolverMcpModule,
    rules::{
        execute::{prepare, run},
        manifest::{manifest_conformance_vectors, manifest_family_executable_rule_ids},
    },
    service::SolverService,
};

const DATA_INTEGRITY_RULE_IDS: [&str; 8] = [
    "data_integrity.aggregate_balance",
    "data_integrity.cardinality",
    "data_integrity.conditional_required",
    "data_integrity.foreign_key",
    "data_integrity.mutually_consistent",
    "data_integrity.temporal_consistency",
    "data_integrity.unique",
    "data_integrity.value_range",
];

fn context() -> ToolCallContext<'static> {
    ToolCallContext::new(ServerKind::Brain, ToolAuthority::Brain, None, None)
}

fn registry() -> ToolRegistry {
    ToolRegistry::builder()
        .with(SolverMcpModule::new(Arc::new(SolverService::new())))
        .expect("register live solver module")
        .build()
}

fn result_json(response: &spur_mcp::response::JsonRpcResponse) -> Value {
    assert!(
        response.error.is_none(),
        "unexpected MCP error: {:?}",
        response.error
    );
    let text = response
        .result
        .as_ref()
        .and_then(|result| result["content"][0]["text"].as_str())
        .expect("MCP JSON text result");
    serde_json::from_str(text).expect("parse MCP JSON text")
}

async fn solve(registry: &ToolRegistry, request: &Value) -> Value {
    result_json(
        &registry
            .call_json_tool(context(), "solve_rules", request.clone())
            .await,
    )
}

async fn solve_direct(request: &Value) -> Value {
    let prepared = prepare(request.clone()).expect("manifest request must compile directly");
    let result = run(&SolverService::new(), prepared)
        .await
        .expect("manifest request must execute directly");
    serde_json::to_value(result).expect("serialize direct solve_rules result")
}

fn expected_constraint_id(rule_id: &str, request: &Value) -> String {
    let subject = request["rules"][0]["subjects"][0]
        .as_str()
        .expect("single manifest definition subject");
    format!(
        "data_integrity_rule_0_{}__subject_{subject}",
        rule_id.replace('.', "_")
    )
}

fn assert_valid_result(rule_id: &str, result: &Value) {
    assert_eq!(result["family"], "data_integrity", "{rule_id} family");
    assert_eq!(result["mode"], "verify", "{rule_id} mode");
    assert_eq!(result["status"], "sat", "{rule_id} status");
    assert_eq!(result["outcome"], "pass", "{rule_id} outcome");
    assert_eq!(
        result["rule_results"],
        json!([{
            "rule_id": rule_id,
            "binding_index": 0,
            "status": "sat",
            "outcome": "pass"
        }]),
        "{rule_id} pass attribution"
    );
}

fn assert_invalid_result(
    rule_id: &str,
    expected_diagnostic: &str,
    request: &Value,
    result: &Value,
) {
    assert_eq!(result["family"], "data_integrity", "{rule_id} family");
    assert_eq!(result["mode"], "verify", "{rule_id} mode");
    assert_eq!(result["status"], "unsat", "{rule_id} status");
    assert_eq!(result["outcome"], "fail", "{rule_id} outcome");
    assert_eq!(
        result["rule_results"],
        json!([{
            "rule_id": rule_id,
            "binding_index": 0,
            "status": "unsat",
            "outcome": "fail",
            "diagnostic": expected_diagnostic
        }]),
        "{rule_id} rejection attribution"
    );
    assert_eq!(
        result["unsat_core"],
        json!([expected_constraint_id(rule_id, request)]),
        "{rule_id} definition-subject core attribution"
    );
}

fn empty_facts() -> Value {
    json!({
        "relations": {},
        "unique_constraints": {},
        "foreign_keys": {},
        "cardinality_constraints": {},
        "value_ranges": {},
        "conditional_requirements": {},
        "aggregate_balances": {},
        "consistency_relations": {},
        "temporal_constraints": {}
    })
}

fn synthesis_request() -> Value {
    let mut facts = empty_facts();
    facts["relations"] = json!({
        "records": {
            "fields": {
                "reading": {"kind": "integer", "minimum": 0, "maximum": 20},
                "state": {"kind": "enum", "values": ["draft", "live"]},
                "enabled": {"kind": "boolean"},
                "trigger": {"kind": "boolean"},
                "required": {"kind": "boolean"}
            },
            "rows": {
                "first": {
                    "active": null,
                    "cells": {
                        "reading": {"present": true, "value": null},
                        "state": {"present": true, "value": null},
                        "enabled": {"present": true, "value": null},
                        "trigger": {"present": true, "value": true},
                        "required": {"present": null, "value": true}
                    }
                }
            }
        }
    });
    facts["cardinality_constraints"] = json!({
        "exactly_one": {"relation": "records", "minimum": 1, "maximum": 1}
    });
    facts["value_ranges"] = json!({
        "reading_range": {
            "relation": "records",
            "field": "reading",
            "minimum": 5,
            "maximum": 10
        }
    });
    facts["conditional_requirements"] = json!({
        "required_when_triggered": {
            "relation": "records",
            "trigger_field": "trigger",
            "expected": true,
            "required_field": "required"
        }
    });
    facts["consistency_relations"] = json!({
        "state_enabled": {
            "relation": "records",
            "fields": ["state", "enabled"],
            "allowed": [["live", true]]
        }
    });

    json!({
        "family": "data_integrity",
        "mode": "synthesize",
        "rules": [
            {"rule_id": "data_integrity.cardinality", "subjects": ["exactly_one"], "parameters": {}},
            {"rule_id": "data_integrity.value_range", "subjects": ["reading_range"], "parameters": {}},
            {"rule_id": "data_integrity.conditional_required", "subjects": ["required_when_triggered"], "parameters": {}},
            {"rule_id": "data_integrity.mutually_consistent", "subjects": ["state_enabled"], "parameters": {}}
        ],
        "facts": facts,
        "unknowns": [
            {"kind": "cell_value", "relation": "records", "row": "first", "field": "state"},
            {"kind": "row_active", "relation": "records", "row": "first"},
            {"kind": "cell_present", "relation": "records", "row": "first", "field": "required"},
            {"kind": "cell_value", "relation": "records", "row": "first", "field": "enabled"},
            {"kind": "cell_value", "relation": "records", "row": "first", "field": "reading"}
        ]
    })
}

fn one_cell_request(mode: &str, active: Value, unknowns: Value) -> Value {
    let mut facts = empty_facts();
    facts["relations"] = json!({
        "records": {
            "fields": {"key": {"kind": "integer", "minimum": 0, "maximum": 9}},
            "rows": {
                "first": {
                    "active": active,
                    "cells": {"key": {"present": true, "value": 1}}
                }
            }
        }
    });
    facts["unique_constraints"] = json!({"record_key": {"relation": "records", "fields": ["key"]}});
    json!({
        "family": "data_integrity",
        "mode": mode,
        "rules": [{
            "rule_id": "data_integrity.unique",
            "subjects": ["record_key"],
            "parameters": {}
        }],
        "facts": facts,
        "unknowns": unknowns
    })
}

fn all_eight_request() -> Value {
    let mut facts = empty_facts();
    facts["relations"] = json!({
        "records": {
            "fields": {
                "id": {"kind": "integer", "minimum": 0, "maximum": 100},
                "parent_id": {"kind": "integer", "minimum": 0, "maximum": 100},
                "count": {"kind": "integer", "minimum": 0, "maximum": 100},
                "status": {"kind": "enum", "values": ["draft", "live"]},
                "enabled": {"kind": "boolean"},
                "required": {"kind": "boolean"},
                "start": {"kind": "integer", "minimum": 0, "maximum": 100},
                "end": {"kind": "integer", "minimum": 0, "maximum": 100}
            },
            "rows": {
                "first": {
                    "active": true,
                    "cells": {
                        "id": {"present": true, "value": 1},
                        "parent_id": {"present": false, "value": null},
                        "count": {"present": true, "value": 1},
                        "status": {"present": true, "value": "draft"},
                        "enabled": {"present": true, "value": false},
                        "required": {"present": true, "value": true},
                        "start": {"present": true, "value": 0},
                        "end": {"present": true, "value": 4}
                    }
                },
                "second": {
                    "active": true,
                    "cells": {
                        "id": {"present": true, "value": 2},
                        "parent_id": {"present": true, "value": 1},
                        "count": {"present": true, "value": 2},
                        "status": {"present": true, "value": "live"},
                        "enabled": {"present": true, "value": true},
                        "required": {"present": true, "value": true},
                        "start": {"present": true, "value": 4},
                        "end": {"present": true, "value": 6}
                    }
                }
            }
        }
    });
    facts["unique_constraints"] = json!({"record_key": {"relation": "records", "fields": ["id"]}});
    facts["foreign_keys"] = json!({
        "record_parent": {
            "child_relation": "records",
            "child_fields": ["parent_id"],
            "parent_relation": "records",
            "parent_fields": ["id"]
        }
    });
    facts["cardinality_constraints"] = json!({
        "record_count": {"relation": "records", "minimum": 1, "maximum": 2}
    });
    facts["value_ranges"] = json!({
        "count_range": {"relation": "records", "field": "count", "minimum": 0, "maximum": 10}
    });
    facts["conditional_requirements"] = json!({
        "required_when_live": {
            "relation": "records",
            "trigger_field": "status",
            "expected": "live",
            "required_field": "required"
        }
    });
    facts["aggregate_balances"] = json!({
        "count_total": {
            "terms": [
                {"relation": "records", "row": "first", "field": "count", "coefficient": 1},
                {"relation": "records", "row": "second", "field": "count", "coefficient": 1}
            ],
            "target": 3
        }
    });
    facts["consistency_relations"] = json!({
        "status_enabled": {
            "relation": "records",
            "fields": ["status", "enabled"],
            "allowed": [["draft", false], ["live", true]]
        }
    });
    facts["temporal_constraints"] = json!({
        "ordered_records": {
            "relation": "records",
            "start_field": "start",
            "end_field": "end",
            "predecessors": [{"before": "first", "after": "second"}]
        }
    });

    json!({
        "family": "data_integrity",
        "mode": "verify",
        "rules": [
            {"rule_id": "data_integrity.unique", "subjects": ["record_key"], "parameters": {}},
            {"rule_id": "data_integrity.foreign_key", "subjects": ["record_parent"], "parameters": {}},
            {"rule_id": "data_integrity.cardinality", "subjects": ["record_count"], "parameters": {}},
            {"rule_id": "data_integrity.value_range", "subjects": ["count_range"], "parameters": {}},
            {"rule_id": "data_integrity.conditional_required", "subjects": ["required_when_live"], "parameters": {}},
            {"rule_id": "data_integrity.aggregate_balance", "subjects": ["count_total"], "parameters": {}},
            {"rule_id": "data_integrity.mutually_consistent", "subjects": ["status_enabled"], "parameters": {}},
            {"rule_id": "data_integrity.temporal_consistency", "subjects": ["ordered_records"], "parameters": {}}
        ],
        "facts": facts,
        "unknowns": []
    })
}

fn budget_request(allowed_in_last_definition: usize) -> Value {
    const DEFINITIONS: usize = 256;
    const TUPLES_PER_DEFINITION: usize = 64;

    let label_count = allowed_in_last_definition.max(TUPLES_PER_DEFINITION);
    let labels = (0..label_count)
        .map(|index| format!("label_{index}"))
        .collect::<Vec<_>>();
    let definitions = (0..DEFINITIONS)
        .map(|index| {
            let allowed_count = if index + 1 == DEFINITIONS {
                allowed_in_last_definition
            } else {
                TUPLES_PER_DEFINITION
            };
            let allowed = labels
                .iter()
                .take(allowed_count)
                .map(|label| json!([label]))
                .collect::<Vec<_>>();
            (
                format!("definition_{index}"),
                json!({
                    "relation": "records",
                    "fields": ["state"],
                    "allowed": allowed
                }),
            )
        })
        .collect::<Map<_, _>>();

    let mut facts = empty_facts();
    facts["relations"] = json!({
        "records": {
            "fields": {"state": {"kind": "enum", "values": labels}},
            "rows": {
                "only": {
                    "active": true,
                    "cells": {"state": {"present": true, "value": "label_0"}}
                }
            }
        }
    });
    facts["consistency_relations"] = Value::Object(definitions);
    json!({
        "family": "data_integrity",
        "mode": "verify",
        "rules": [{
            "rule_id": "data_integrity.mutually_consistent",
            "subjects": ["definition_0"],
            "parameters": {}
        }],
        "facts": facts,
        "unknowns": []
    })
}

#[tokio::test]
async fn every_manifest_vector_is_double_evaluated_with_exact_attribution() {
    let rule_ids = manifest_family_executable_rule_ids("data_integrity")
        .expect("registered data_integrity executable rules");
    assert_eq!(rule_ids, DATA_INTEGRITY_RULE_IDS);
    let registry = registry();

    for rule_id in rule_ids {
        let vectors = manifest_conformance_vectors(rule_id)
            .unwrap_or_else(|| panic!("missing conformance vectors for `{rule_id}`"));
        assert_eq!(vectors.valid.len(), 1, "{rule_id} valid vector count");
        assert_eq!(vectors.invalid.len(), 1, "{rule_id} invalid vector count");

        let valid = &vectors.valid[0];
        assert_eq!(
            valid.expected_diagnostic, None,
            "{rule_id} valid manifest expectation"
        );
        let direct_valid = solve_direct(&valid.request).await;
        let mcp_valid = solve(&registry, &valid.request).await;
        assert_valid_result(rule_id, &direct_valid);
        assert_valid_result(rule_id, &mcp_valid);
        assert_eq!(
            direct_valid["rule_results"], mcp_valid["rule_results"],
            "{rule_id} valid direct/MCP agreement"
        );

        let invalid = &vectors.invalid[0];
        let expected_diagnostic = invalid
            .expected_diagnostic
            .as_deref()
            .expect("invalid manifest diagnostic expectation");
        assert_eq!(
            expected_diagnostic,
            format!("{rule_id}.violation"),
            "{rule_id} exact manifest diagnostic"
        );
        let direct_invalid = solve_direct(&invalid.request).await;
        let mcp_invalid = solve(&registry, &invalid.request).await;
        assert_invalid_result(
            rule_id,
            expected_diagnostic,
            &invalid.request,
            &direct_invalid,
        );
        assert_invalid_result(rule_id, expected_diagnostic, &invalid.request, &mcp_invalid);
        assert_eq!(
            direct_invalid["rule_results"], mcp_invalid["rule_results"],
            "{rule_id} invalid direct/MCP agreement"
        );
    }
}

#[tokio::test]
async fn synthesis_projects_all_unknown_kinds_and_typed_values_in_caller_order() {
    let result = solve(&registry(), &synthesis_request()).await;
    assert_eq!(result["status"], "sat");
    assert_eq!(result["outcome"], "solution");
    let assignments = result["assignments"]
        .as_array()
        .expect("data integrity synthesis assignments");
    assert_eq!(
        assignments
            .iter()
            .map(|assignment| assignment["field"].as_str().expect("assignment field"))
            .collect::<Vec<_>>(),
        [
            "rows.first.cells.state.value",
            "rows.first.active",
            "rows.first.cells.required.present",
            "rows.first.cells.enabled.value",
            "rows.first.cells.reading.value",
        ]
    );
    assert!(assignments
        .iter()
        .all(|assignment| assignment["node"] == "records"));
    assert_eq!(assignments[0]["value"], "live", "enum cell_value");
    assert_eq!(assignments[1]["value"], 1, "row_active Boolean encoding");
    assert_eq!(assignments[2]["value"], 1, "cell_present Boolean encoding");
    assert_eq!(assignments[3]["value"], 1, "Boolean cell_value encoding");
    assert!(
        assignments[4]["value"]
            .as_i64()
            .is_some_and(|value| (5..=10).contains(&value)),
        "integer cell_value: {}",
        assignments[4]
    );
}

#[tokio::test]
async fn verify_mode_rejects_declared_unknowns_and_incomplete_required_facts() {
    let registry = registry();
    let declared_unknown = registry
        .call_json_tool(
            context(),
            "solve_rules",
            one_cell_request(
                "verify",
                Value::Null,
                json!([{"kind": "row_active", "relation": "records", "row": "first"}]),
            ),
        )
        .await;
    let error = declared_unknown
        .error
        .expect("verify mode must reject declared unknowns");
    assert_eq!(error.code, -32602);
    assert!(
        error.message.contains(
            "verification requires complete data integrity facts; remove unknown declarations"
        ),
        "{}",
        error.message
    );

    let incomplete = registry
        .call_json_tool(
            context(),
            "solve_rules",
            one_cell_request("verify", Value::Null, json!([])),
        )
        .await;
    let error = incomplete
        .error
        .expect("verify mode must reject incomplete required facts");
    assert_eq!(error.code, -32602);
    assert!(
        error
            .message
            .contains("null `records.first.active` requires a row_active unknown"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn all_eight_rules_compose_and_conflict_remains_attributable() {
    let registry = registry();
    let request = all_eight_request();
    let valid = solve(&registry, &request).await;
    assert_eq!(valid["status"], "sat");
    assert_eq!(valid["outcome"], "pass");
    let valid_results = valid["rule_results"]
        .as_array()
        .expect("all-eight pass attribution");
    assert_eq!(valid_results.len(), 8);
    assert!(valid_results
        .iter()
        .all(|result| result["outcome"] == "pass"));

    let mut conflict = request;
    conflict["facts"]["relations"]["records"]["rows"]["second"]["cells"]["id"]["value"] = json!(1);
    let invalid = solve(&registry, &conflict).await;
    assert_eq!(invalid["status"], "unsat");
    assert_eq!(invalid["outcome"], "fail");
    let results = invalid["rule_results"]
        .as_array()
        .expect("all-eight conflict attribution");
    assert_eq!(results.len(), 8);
    for (index, result) in results.iter().enumerate() {
        assert_eq!(result["binding_index"], index, "binding {index}");
        if index == 0 {
            assert_eq!(result["rule_id"], "data_integrity.unique");
            assert_eq!(result["status"], "unsat");
            assert_eq!(result["outcome"], "fail");
            assert_eq!(result["diagnostic"], "data_integrity.unique.violation");
        } else {
            assert_eq!(result["outcome"], "pass", "binding {index}: {result}");
        }
    }
    assert_eq!(
        invalid["unsat_core"],
        json!(["data_integrity_rule_0_data_integrity_unique__subject_record_key"])
    );
}

#[test]
fn finite_expansion_budget_accepts_below_and_exact_limit_then_rejects_excess() {
    const DEFINITIONS: usize = 256;
    const BELOW_TUPLES: usize = 63;
    const EXACT_TUPLES: usize = 64;
    const ABOVE_TUPLES: usize = 65;

    let below_estimate = (DEFINITIONS - 1) * EXACT_TUPLES + BELOW_TUPLES;
    let exact_estimate = DEFINITIONS * EXACT_TUPLES;
    let above_estimate = (DEFINITIONS - 1) * EXACT_TUPLES + ABOVE_TUPLES;
    assert_eq!(below_estimate, 16_383);
    assert_eq!(exact_estimate, 16_384);
    assert_eq!(above_estimate, 16_385);

    prepare(budget_request(BELOW_TUPLES)).expect("16,383-node expansion must be accepted");
    prepare(budget_request(EXACT_TUPLES)).expect("16,384-node expansion must be accepted");
    let error = prepare(budget_request(ABOVE_TUPLES))
        .expect_err("16,385-node expansion must be rejected")
        .to_string();
    assert!(
        error.contains("data integrity expression estimate 16385 exceeds 16384 nodes"),
        "{error}"
    );

    assert_eq!(
        usize::MAX
            .checked_mul(2)
            .and_then(|value| value.checked_mul(2)),
        None,
        "the host-width arithmetic-overflow boundary used by the checked estimator"
    );
}
