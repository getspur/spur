use std::{
    fs,
    path::{Path, PathBuf},
};

#[expect(dead_code)]
#[path = "../src/rules/manifest_format.rs"]
mod manifest_format;
#[path = "../build_support/manifest_source.rs"]
mod manifest_source;

use manifest_format::{
    AvailabilityV1, ManifestBundleV1, NativeHandlerV1, RuleStrengthV1, SubjectCardinalityV1,
};
use manifest_source::{
    canonical_manifest_json, load_manifest_sources, manifest_rerun_paths, write_canonical_manifest,
};

fn repository_manifest_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/rules/families")
}

fn relative_paths(root: &Path, paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| {
            path.strip_prefix(root)
                .expect("manifest source must be below its root")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

fn copy_repository_sources(destination: &Path) -> PathBuf {
    let source_root = repository_manifest_root();
    let loaded = load_manifest_sources(&source_root).expect("repository manifests must load");
    let destination_root = destination.join("families");

    for source in loaded.source_paths {
        let relative = source
            .strip_prefix(&source_root)
            .expect("manifest source must be below its root");
        let target = destination_root.join(relative);
        fs::create_dir_all(target.parent().expect("manifest source parent"))
            .expect("create temporary manifest directory");
        fs::copy(&source, &target).expect("copy repository manifest source");
    }

    destination_root
}

fn replace_source(root: &Path, relative: &str, from: &str, to: &str) {
    let path = root.join(relative);
    let source = fs::read_to_string(&path).expect("read copied manifest source");
    let changed = source.replacen(from, to, 1);
    assert_ne!(changed, source, "replacement must modify {relative}");
    fs::write(path, changed).expect("write modified manifest source");
}

fn assert_complete_data_integrity_request(rule_id: &str, request: &serde_json::Value) {
    let request = request
        .as_object()
        .unwrap_or_else(|| panic!("{rule_id} request must be an object"));
    let request_keys = request
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        request_keys,
        ["facts", "family", "mode", "rules", "unknowns"]
            .into_iter()
            .collect(),
        "{rule_id} request keys"
    );
    assert_eq!(request["family"], "data_integrity", "{rule_id} family");
    assert_eq!(request["mode"], "verify", "{rule_id} mode");
    assert_eq!(
        request["unknowns"],
        serde_json::json!([]),
        "{rule_id} unknowns"
    );

    let bindings = request["rules"]
        .as_array()
        .unwrap_or_else(|| panic!("{rule_id} rules must be an array"));
    assert_eq!(bindings.len(), 1, "{rule_id} must bind exactly one rule");
    assert_eq!(bindings[0]["rule_id"], rule_id, "{rule_id} binding");
    assert_eq!(
        bindings[0]["subjects"].as_array().map(Vec::len),
        Some(1),
        "{rule_id} must bind exactly one definition subject"
    );
    assert_eq!(
        bindings[0]["parameters"],
        serde_json::json!({}),
        "{rule_id} caller parameters"
    );

    let facts = request["facts"]
        .as_object()
        .unwrap_or_else(|| panic!("{rule_id} facts must be an object"));
    let fact_keys = facts
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        fact_keys,
        [
            "aggregate_balances",
            "cardinality_constraints",
            "conditional_requirements",
            "consistency_relations",
            "foreign_keys",
            "relations",
            "temporal_constraints",
            "unique_constraints",
            "value_ranges",
        ]
        .into_iter()
        .collect(),
        "{rule_id} must provide the complete finite-snapshot fact shape"
    );
}

#[test]
fn approved_sources_load_in_deterministic_canonical_order() {
    let root = repository_manifest_root();
    let first = load_manifest_sources(&root).expect("repository manifests must load");
    let second = load_manifest_sources(&root).expect("repository manifests must load repeatedly");

    let expected_paths = [
        "accessibility/family.yaml",
        "accessibility/rules/focus_not_obscured.yaml",
        "accessibility/rules/reflow.yaml",
        "accessibility/rules/target_size.yaml",
        "accessibility/rules/text_contrast.yaml",
        "configuration/family.yaml",
        "configuration/rules/attribute_allowed_pair.yaml",
        "configuration/rules/excludes.yaml",
        "configuration/rules/requires_any.yaml",
        "configuration/rules/selection_cardinality.yaml",
        "configuration/rules/version_interval.yaml",
        "data_integrity/family.yaml",
        "data_integrity/rules/aggregate_balance.yaml",
        "data_integrity/rules/cardinality.yaml",
        "data_integrity/rules/conditional_required.yaml",
        "data_integrity/rules/foreign_key.yaml",
        "data_integrity/rules/mutually_consistent.yaml",
        "data_integrity/rules/temporal_consistency.yaml",
        "data_integrity/rules/unique.yaml",
        "data_integrity/rules/value_range.yaml",
        "design/family.yaml",
        "design/rules/aspect_ratio.yaml",
        "design/rules/axis_capacity.yaml",
        "design/rules/containment.yaml",
        "design/rules/non_overlap.yaml",
        "policy/family.yaml",
        "policy/rules/dynamic_separation_of_duty.yaml",
        "policy/rules/minimum_privilege.yaml",
        "policy/rules/permission_reachable.yaml",
        "policy/rules/role_hierarchy_acyclic.yaml",
        "policy/rules/static_separation_of_duty.yaml",
        "resource/family.yaml",
        "resource/rules/aggregate_capacity.yaml",
        "resource/rules/minimum_failure_domains.yaml",
        "resource/rules/quota_capacity.yaml",
        "resource/rules/request_within_limit.yaml",
        "resource/rules/topology_max_skew.yaml",
        "scheduling/family.yaml",
        "scheduling/rules/assignment_exactly_once.yaml",
        "scheduling/rules/cumulative_capacity.yaml",
        "scheduling/rules/minimize_makespan.yaml",
        "scheduling/rules/placement_allowed.yaml",
        "scheduling/rules/precedence_finish_start.yaml",
        "workflow/family.yaml",
        "workflow/rules/bounded_reachability.yaml",
        "workflow/rules/initial_state_allowed.yaml",
        "workflow/rules/safety_invariant.yaml",
        "workflow/rules/transition_allowed.yaml",
    ];
    assert_eq!(relative_paths(&root, &first.source_paths), expected_paths);
    assert_eq!(first, second);

    let family_ids = first
        .bundle
        .families
        .iter()
        .map(|family| family.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        family_ids,
        [
            "accessibility",
            "configuration",
            "data_integrity",
            "design",
            "policy",
            "resource",
            "scheduling",
            "workflow"
        ]
    );
    assert!(first
        .bundle
        .rules
        .windows(2)
        .all(|rules| rules[0].id < rules[1].id));

    let json = canonical_manifest_json(&first.bundle).expect("serialize canonical bundle");
    assert_eq!(json, canonical_manifest_json(&second.bundle).unwrap());
    assert!(!json.contains('\n'), "canonical JSON must be compact");
    let round_trip: ManifestBundleV1 =
        serde_json::from_str(&json).expect("canonical JSON must deserialize");
    assert_eq!(round_trip, first.bundle);
}

#[test]
fn data_integrity_catalog_and_conformance_match_the_approved_contract() {
    let loaded =
        load_manifest_sources(&repository_manifest_root()).expect("repository manifests must load");
    let family = loaded
        .bundle
        .families
        .iter()
        .find(|family| family.id == "data_integrity")
        .expect("data_integrity family must be discovered");
    let profiles = family
        .profiles
        .iter()
        .map(|profile| (profile.id.as_str(), profile.profile_version))
        .collect::<Vec<_>>();
    assert_eq!(profiles, [("finite_relational_snapshot", 1)]);

    let expected = [
        (
            "data_integrity.aggregate_balance",
            NativeHandlerV1::DataIntegrityAggregateBalance,
        ),
        (
            "data_integrity.cardinality",
            NativeHandlerV1::DataIntegrityCardinality,
        ),
        (
            "data_integrity.conditional_required",
            NativeHandlerV1::DataIntegrityConditionalRequired,
        ),
        (
            "data_integrity.foreign_key",
            NativeHandlerV1::DataIntegrityForeignKey,
        ),
        (
            "data_integrity.mutually_consistent",
            NativeHandlerV1::DataIntegrityMutuallyConsistent,
        ),
        (
            "data_integrity.temporal_consistency",
            NativeHandlerV1::DataIntegrityTemporalConsistency,
        ),
        (
            "data_integrity.unique",
            NativeHandlerV1::DataIntegrityUnique,
        ),
        (
            "data_integrity.value_range",
            NativeHandlerV1::DataIntegrityValueRange,
        ),
    ];
    let rules = loaded
        .bundle
        .rules
        .iter()
        .filter(|rule| rule.family == "data_integrity")
        .collect::<Vec<_>>();
    assert_eq!(
        rules
            .iter()
            .map(|rule| rule.id.as_str())
            .collect::<Vec<_>>(),
        expected.map(|(id, _)| id)
    );

    for (rule, (rule_id, handler)) in rules.iter().zip(expected) {
        assert_eq!(rule.profile, "finite_relational_snapshot", "{rule_id}");
        assert_eq!(rule.availability, AvailabilityV1::Implemented, "{rule_id}");
        assert_eq!(rule.strength, RuleStrengthV1::Hard, "{rule_id}");
        assert_eq!(
            rule.subjects.cardinality,
            SubjectCardinalityV1::Exact { count: 1 },
            "{rule_id} subject ABI"
        );
        assert!(rule.parameters.is_empty(), "{rule_id} parameter ABI");
        assert_eq!(rule.handler, Some(handler), "{rule_id} native handler");
        assert!(
            rule.solver_encoding
                .synthesis
                .contains("explicitly unknown"),
            "{rule_id} must state explicit-only unknown behavior"
        );

        let diagnostic = format!("{rule_id}.violation");
        assert_eq!(rule.examples.valid.expected_diagnostic, None, "{rule_id}");
        assert_eq!(
            rule.examples.invalid.expected_diagnostic.as_deref(),
            Some(diagnostic.as_str()),
            "{rule_id} invalid example diagnostic"
        );
        assert_complete_data_integrity_request(rule_id, &rule.examples.valid.facts);
        assert_complete_data_integrity_request(rule_id, &rule.examples.invalid.facts);

        let conformance = rule
            .conformance
            .as_ref()
            .unwrap_or_else(|| panic!("{rule_id} conformance vectors"));
        assert_eq!(conformance.valid.len(), 1, "{rule_id} valid vectors");
        assert_eq!(conformance.invalid.len(), 1, "{rule_id} invalid vectors");
        assert_eq!(conformance.valid[0].expected_diagnostic, None, "{rule_id}");
        assert_eq!(
            conformance.invalid[0].expected_diagnostic.as_deref(),
            Some(diagnostic.as_str()),
            "{rule_id} invalid conformance diagnostic"
        );
        assert_complete_data_integrity_request(rule_id, &conformance.valid[0].request);
        assert_complete_data_integrity_request(rule_id, &conformance.invalid[0].request);
        assert_eq!(
            rule.examples.valid.facts, conformance.valid[0].request,
            "{rule_id} valid catalog and executable requests must match"
        );
        assert_eq!(
            rule.examples.invalid.facts, conformance.invalid[0].request,
            "{rule_id} invalid catalog and executable requests must match"
        );
    }

    let rule = |id: &str| {
        rules
            .iter()
            .copied()
            .find(|rule| rule.id == id)
            .unwrap_or_else(|| panic!("missing {id}"))
    };

    let unique = rule("data_integrity.unique");
    assert_eq!(unique.primitive, "finite_unique_nulls_distinct");
    assert_eq!(
        unique
            .examples
            .valid
            .facts
            .pointer("/facts/relations/records/rows/null_key/cells/key"),
        Some(&serde_json::json!({"present": false, "value": null})),
        "NULLS DISTINCT needs an absent-key witness"
    );

    let foreign_key = rule("data_integrity.foreign_key");
    assert_eq!(foreign_key.primitive, "finite_foreign_key_match_simple");
    assert_eq!(
        foreign_key
            .examples
            .valid
            .facts
            .pointer("/facts/relations/children/rows/absent/cells/parent_key"),
        Some(&serde_json::json!({"present": false, "value": null})),
        "MATCH SIMPLE needs an incomplete-child-key witness"
    );

    let cardinality = rule("data_integrity.cardinality");
    assert_eq!(
        cardinality.solver_encoding.formula,
        ["minimum <= sum over declared rows of ite(active(row), 1, 0) <= maximum"]
    );

    let value_range = rule("data_integrity.value_range");
    assert_eq!(
        value_range
            .examples
            .valid
            .facts
            .pointer("/facts/relations/measurements/rows/observed/cells/measured/value"),
        Some(&serde_json::json!(100)),
        "inclusive range needs an upper-bound witness"
    );
    assert_eq!(
        value_range.solver_encoding.formula,
        ["active(row) and present(row,field) implies minimum <= value(row,field) <= maximum"]
    );

    let aggregate = rule("data_integrity.aggregate_balance");
    assert_eq!(
        aggregate.summary,
        "Require every declared integer term cell to be present and resolvable and every checked linear term to contribute to one exact integer target."
    );
    assert_eq!(
        aggregate.llm_encoding.encode_steps,
        [
            "Bind exactly one aggregate-balance definition ID as the subject.",
            "Resolve every explicitly listed term to a present integer cell and validate each checked integer coefficient.",
            "Include every explicitly listed term in the exact weighted sum and equate it to the target.",
        ]
    );
    assert_eq!(
        aggregate.solver_encoding.verification,
        "fix every listed integer cell and assert unconditional presence plus exact weighted equality"
    );
    assert_eq!(
        aggregate.solver_encoding.formula,
        [
            "for every listed term t: present(t)",
            "sum over every listed term t of coefficient(t) * value(t) = target",
        ]
    );
    assert!(
        aggregate
            .llm_encoding
            .anti_patterns
            .iter()
            .any(|pattern| pattern
                == "Do not condition term presence or arithmetic contribution on row activity."),
        "aggregate guidance must explicitly reject activity filtering"
    );
    let positive_semantics = format!(
        "{} {} {}",
        aggregate.summary,
        aggregate.llm_encoding.encode_steps.join(" "),
        aggregate.solver_encoding.formula.join(" ")
    );
    assert!(
        !positive_semantics.contains("active") && !positive_semantics.contains("activity"),
        "aggregate presence and contribution semantics must be unconditional"
    );
    assert_eq!(
        aggregate
            .examples
            .valid
            .facts
            .pointer("/facts/aggregate_balances/ledger_balance"),
        Some(&serde_json::json!({
            "terms": [
                {"relation": "ledger", "row": "left", "field": "amount", "coefficient": 1},
                {"relation": "ledger", "row": "delta", "field": "amount", "coefficient": 1},
                {"relation": "ledger", "row": "right", "field": "amount", "coefficient": -1}
            ],
            "target": 0
        })),
        "aggregate terms and coefficients must remain exact"
    );
    assert_eq!(
        aggregate
            .examples
            .valid
            .facts
            .pointer("/facts/relations/ledger/rows/delta"),
        Some(&serde_json::json!({
            "active": false,
            "cells": {"amount": {"present": true, "value": -30}}
        })),
        "an inactive row's present value must witness unconditional contribution"
    );
    let valid_request = &aggregate.examples.valid.facts;
    let balance = valid_request
        .pointer("/facts/aggregate_balances/ledger_balance")
        .expect("aggregate balance definition");
    let target = balance["target"].as_i64().expect("integer target");
    let terms = balance["terms"].as_array().expect("aggregate terms");
    let exact_sum = terms
        .iter()
        .map(|term| {
            let relation = term["relation"].as_str().expect("term relation");
            let row = term["row"].as_str().expect("term row");
            let field = term["field"].as_str().expect("term field");
            let coefficient = term["coefficient"].as_i64().expect("term coefficient");
            let value = valid_request
                .pointer(&format!(
                    "/facts/relations/{relation}/rows/{row}/cells/{field}/value"
                ))
                .and_then(serde_json::Value::as_i64)
                .expect("listed term integer value");
            coefficient * value
        })
        .sum::<i64>();
    let activity_filtered_sum = terms
        .iter()
        .filter(|term| {
            let relation = term["relation"].as_str().expect("term relation");
            let row = term["row"].as_str().expect("term row");
            valid_request
                .pointer(&format!("/facts/relations/{relation}/rows/{row}/active"))
                .and_then(serde_json::Value::as_bool)
                .expect("row activity")
        })
        .map(|term| {
            let relation = term["relation"].as_str().expect("term relation");
            let row = term["row"].as_str().expect("term row");
            let field = term["field"].as_str().expect("term field");
            let coefficient = term["coefficient"].as_i64().expect("term coefficient");
            let value = valid_request
                .pointer(&format!(
                    "/facts/relations/{relation}/rows/{row}/cells/{field}/value"
                ))
                .and_then(serde_json::Value::as_i64)
                .expect("listed term integer value");
            coefficient * value
        })
        .sum::<i64>();
    assert_eq!(
        exact_sum, target,
        "every explicitly listed term contributes"
    );
    assert_ne!(
        activity_filtered_sum, target,
        "the valid witness must fail if row activity is reintroduced as a filter"
    );

    let consistency = rule("data_integrity.mutually_consistent");
    assert_eq!(
        consistency
            .examples
            .valid
            .facts
            .pointer("/facts/consistency_relations/plan_region/allowed"),
        Some(&serde_json::json!([
            ["free", "us"],
            ["pro", "us"],
            ["pro", "eu"]
        ])),
        "the allowed finite tuple relation must remain closed and ordered"
    );

    let temporal = rule("data_integrity.temporal_consistency");
    assert_eq!(
        temporal.solver_encoding.formula,
        [
            "active(row) implies present(start) and present(end) and start < end",
            "for every before -> after edge: active(after) implies active(before) and end(before) <= start(after)",
        ]
    );
    assert_eq!(
        temporal
            .examples
            .valid
            .facts
            .pointer("/facts/relations/tasks/rows/predecessor/cells/end/value"),
        temporal
            .examples
            .valid
            .facts
            .pointer("/facts/relations/tasks/rows/successor/cells/start/value"),
        "finish-start ordering must allow an inclusive meeting boundary"
    );
}

#[test]
fn workflow_catalog_uses_approved_profile_and_rule_order() {
    let root = repository_manifest_root();
    let loaded = load_manifest_sources(&root).expect("repository manifests must load");
    let family = loaded
        .bundle
        .families
        .iter()
        .find(|family| family.id == "workflow")
        .expect("workflow family must be discovered");
    let profile_ids = family
        .profiles
        .iter()
        .map(|profile| profile.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(profile_ids, ["bounded_trace"]);

    let rule_ids = loaded
        .bundle
        .rules
        .iter()
        .filter(|rule| rule.family == "workflow")
        .map(|rule| rule.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        rule_ids,
        [
            "workflow.bounded_reachability",
            "workflow.initial_state_allowed",
            "workflow.safety_invariant",
            "workflow.transition_allowed",
        ]
    );
}

#[test]
fn discovery_ignores_yaml_outside_approved_source_paths() {
    let temp = tempfile::tempdir().expect("temporary manifest root");
    let root = copy_repository_sources(temp.path());
    let expected = load_manifest_sources(&root).expect("copied manifests must load");

    fs::write(root.join("accessibility/notes.yaml"), "not: a manifest\n")
        .expect("write unapproved family-side YAML");
    fs::write(
        root.join("accessibility/rules/notes.yml"),
        "not: a manifest\n",
    )
    .expect("write unapproved extension");
    fs::create_dir_all(root.join("accessibility/rules/nested"))
        .expect("create unapproved nested rules directory");
    fs::write(
        root.join("accessibility/rules/nested/notes.yaml"),
        "not: a manifest\n",
    )
    .expect("write unapproved nested YAML");
    fs::create_dir_all(root.join("unapproved/rules")).expect("create unapproved family directory");
    fs::write(root.join("unapproved/family.yaml"), "not: a manifest\n")
        .expect("write unapproved family YAML");

    let actual = load_manifest_sources(&root).expect("unapproved YAML must be ignored");
    assert_eq!(actual, expected);
}

#[test]
fn strict_parse_failures_include_source_and_rule_context() {
    let temp = tempfile::tempdir().expect("temporary manifest root");
    let root = copy_repository_sources(temp.path());
    let relative = "accessibility/rules/target_size.yaml";
    let path = root.join(relative);
    let source = fs::read_to_string(&path).expect("read copied rule");
    fs::write(&path, format!("{source}unknown_field: true\n")).expect("malform copied rule");

    let error = load_manifest_sources(&root).expect_err("unknown fields must fail strictly");
    let diagnostic = error.to_string();
    assert!(diagnostic.contains(relative), "{diagnostic}");
    assert!(diagnostic.contains("rule `target_size`"), "{diagnostic}");
    assert!(diagnostic.contains("unknown field"), "{diagnostic}");
}

#[test]
fn cross_document_failures_include_source_and_rule_id() {
    let temp = tempfile::tempdir().expect("temporary manifest root");
    let root = copy_repository_sources(temp.path());
    let relative = "accessibility/rules/target_size.yaml";
    let path = root.join(relative);
    let source = fs::read_to_string(&path).expect("read copied rule");
    fs::write(
        &path,
        source.replacen("family: \"accessibility\"", "family: \"missing\"", 1),
    )
    .expect("break copied rule ownership");

    let error = load_manifest_sources(&root).expect_err("unknown family must fail");
    let diagnostic = error.to_string();
    assert!(diagnostic.contains(relative), "{diagnostic}");
    assert!(
        diagnostic.contains("rule `a11y.target_size`"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains("unknown family `missing`"),
        "{diagnostic}"
    );
}

#[test]
fn implemented_handlers_must_form_a_bijection_with_native_handler_all() {
    let temp = tempfile::tempdir().expect("temporary manifest root");
    let root = copy_repository_sources(temp.path());
    fs::remove_file(root.join("accessibility/rules/focus_not_obscured.yaml"))
        .expect("remove one implemented manifest");

    let error = load_manifest_sources(&root).expect_err("missing native handler must fail");
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("manifest bundle"), "{diagnostic}");
    assert!(
        diagnostic.contains("a11y_focus_not_obscured"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("missing"), "{diagnostic}");
}

#[test]
fn native_handlers_must_belong_to_the_declared_rule_family() {
    let temp = tempfile::tempdir().expect("temporary manifest root");
    let root = copy_repository_sources(temp.path());
    replace_source(
        &root,
        "accessibility/rules/focus_not_obscured.yaml",
        "handler: \"a11y_focus_not_obscured\"",
        "handler: \"rbac_role_hierarchy_acyclic\"",
    );
    replace_source(
        &root,
        "policy/rules/role_hierarchy_acyclic.yaml",
        "handler: rbac_role_hierarchy_acyclic",
        "handler: a11y_focus_not_obscured",
    );

    let diagnostic = load_manifest_sources(&root)
        .expect_err("cross-family native handlers must fail")
        .to_string();
    assert!(diagnostic.contains("handler"), "{diagnostic}");
    assert!(diagnostic.contains("family"), "{diagnostic}");
}

#[test]
fn native_handler_defaulted_parameters_must_keep_a_default() {
    let temp = tempfile::tempdir().expect("temporary manifest root");
    let root = copy_repository_sources(temp.path());
    replace_source(
        &root,
        "resource/rules/topology_max_skew.yaml",
        "    default: 1\n",
        "",
    );

    let diagnostic = load_manifest_sources(&root)
        .expect_err("native handler default drift must fail")
        .to_string();
    assert!(diagnostic.contains("max_skew"), "{diagnostic}");
    assert!(diagnostic.contains("default"), "{diagnostic}");
}

#[test]
fn native_handler_required_parameters_must_remain_required() {
    let temp = tempfile::tempdir().expect("temporary manifest root");
    let root = copy_repository_sources(temp.path());
    replace_source(
        &root,
        "design/rules/axis_capacity.yaml",
        "    required: true\n    kind: \"string_enum\"",
        "    required: false\n    kind: \"string_enum\"",
    );

    let diagnostic = load_manifest_sources(&root)
        .expect_err("native handler required-parameter drift must fail")
        .to_string();
    assert!(diagnostic.contains("axis"), "{diagnostic}");
    assert!(diagnostic.contains("required"), "{diagnostic}");
}

#[test]
fn conformance_diagnostics_must_match_the_rule_violation_diagnostic() {
    let temp = tempfile::tempdir().expect("temporary manifest root");
    let root = copy_repository_sources(temp.path());
    replace_source(
        &root,
        "resource/rules/request_within_limit.yaml",
        "      expected_diagnostic: resource.request_within_limit.violation",
        "      expected_diagnostic: wrong.violation",
    );

    let diagnostic = load_manifest_sources(&root)
        .expect_err("conformance diagnostic drift must fail")
        .to_string();
    assert!(diagnostic.contains("expected_diagnostic"), "{diagnostic}");
    assert!(diagnostic.contains("wrong.violation"), "{diagnostic}");
}

#[test]
fn canonical_output_and_rerun_paths_cover_every_source() {
    let root = repository_manifest_root();
    let loaded = load_manifest_sources(&root).expect("repository manifests must load");
    let rerun_paths = manifest_rerun_paths(&root, &loaded.source_paths);
    for source in &loaded.source_paths {
        assert!(
            rerun_paths.contains(source),
            "missing rerun path: {}",
            source.display()
        );
    }
    for family in [
        "accessibility",
        "configuration",
        "data_integrity",
        "design",
        "policy",
        "resource",
        "scheduling",
        "workflow",
    ] {
        assert!(rerun_paths.contains(&root.join(family)));
        assert!(rerun_paths.contains(&root.join(family).join("rules")));
    }

    let temp = tempfile::tempdir().expect("temporary output directory");
    let output = temp.path().join("spur_rule_manifests_v1.json");
    write_canonical_manifest(&output, &loaded.bundle).expect("write canonical manifest bundle");
    assert_eq!(
        fs::read_to_string(output).expect("read canonical manifest bundle"),
        canonical_manifest_json(&loaded.bundle).expect("serialize canonical manifest bundle")
    );
}
