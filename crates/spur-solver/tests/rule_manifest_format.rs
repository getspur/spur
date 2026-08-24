mod support;

use serde_json::json;
use spur_solver::rules::manifest_format::{
    validate_manifest_bundle, validate_rule_manifest, AvailabilityV1, ExecutionKindV1,
    FamilyManifestV1, ManifestRouteV1, NativeHandlerV1, NativeObjectValidatorV1,
    ParameterContractV1, ParameterKindV1, RuleManifestV1, RuleStrengthV1, SubjectCardinalityV1,
};

use support::{bundle_fixture, conformance_fixture, family_fixture, rule_fixture};

#[test]
fn strict_v1_documents_reject_unknown_fields_and_versions() {
    let yaml = serde_yml::to_string(&family_fixture()).expect("serialize family fixture");
    let round_trip: FamilyManifestV1 =
        serde_yml::from_str(&yaml).expect("parse strict family fixture");
    assert_eq!(round_trip, family_fixture());

    let unknown = format!("{yaml}unexpected_field: true\n");
    assert!(serde_yml::from_str::<FamilyManifestV1>(&unknown).is_err());

    let unsupported = yaml.replacen("schema_version: 1", "schema_version: 2", 1);
    assert!(serde_yml::from_str::<FamilyManifestV1>(&unsupported).is_err());
}

#[test]
fn native_handler_and_object_validator_enums_are_closed() {
    let handler_names = NativeHandlerV1::ALL
        .iter()
        .map(|handler| serde_yml::to_string(handler).expect("serialize handler"))
        .collect::<Vec<_>>();
    assert_eq!(handler_names.len(), 41);
    assert_eq!(
        handler_names
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        41
    );
    assert!(handler_names
        .iter()
        .any(|name| name.trim() == "a11y_target_size"));
    assert!(handler_names
        .iter()
        .any(|name| name.trim() == "rbac_minimum_privilege"));
    assert!(handler_names
        .iter()
        .any(|name| name.trim() == "placement_minimize_skew"));
    assert!(handler_names
        .iter()
        .any(|name| name.trim() == "placement_topology_max_skew"));
    assert!(handler_names
        .iter()
        .any(|name| name.trim() == "scheduling_minimize_makespan"));
    assert!(handler_names
        .iter()
        .any(|name| name.trim() == "workflow_bounded_reachability"));
    assert!(handler_names
        .iter()
        .any(|name| name.trim() == "data_integrity_temporal_consistency"));
    assert!(serde_yml::from_str::<NativeHandlerV1>("crate::rules::compile").is_err());

    assert_eq!(
        NativeObjectValidatorV1::ALL,
        &[NativeObjectValidatorV1::AccessibilityException]
    );
    assert!(serde_yml::from_str::<NativeObjectValidatorV1>("rust_path").is_err());
}

#[test]
fn objective_handlers_follow_family_stable_order() {
    let expected = [
        NativeHandlerV1::RbacDynamicSeparationOfDuty,
        NativeHandlerV1::RbacMinimumPrivilege,
        NativeHandlerV1::RbacPermissionReachable,
        NativeHandlerV1::RbacRoleHierarchyAcyclic,
        NativeHandlerV1::RbacStaticSeparationOfDuty,
        NativeHandlerV1::PlacementMinimumFailureDomains,
        NativeHandlerV1::PlacementMinimizeSkew,
        NativeHandlerV1::PlacementTopologyMaxSkew,
        NativeHandlerV1::ResourceAggregateCapacity,
        NativeHandlerV1::ResourceQuotaCapacity,
        NativeHandlerV1::ResourceRequestWithinLimit,
    ];

    assert_eq!(NativeHandlerV1::ALL.get(8..19), Some(expected.as_slice()));
}

#[test]
fn scheduling_handlers_follow_configuration_in_stable_order() {
    let expected = [
        NativeHandlerV1::ConfigurationRequiresAny,
        NativeHandlerV1::ConfigurationExcludes,
        NativeHandlerV1::ConfigurationSelectionCardinality,
        NativeHandlerV1::ConfigurationAttributeAllowedPair,
        NativeHandlerV1::ConfigurationVersionInterval,
        NativeHandlerV1::SchedulingAssignmentExactlyOnce,
        NativeHandlerV1::SchedulingPlacementAllowed,
        NativeHandlerV1::SchedulingPrecedenceFinishStart,
        NativeHandlerV1::SchedulingCumulativeCapacity,
        NativeHandlerV1::SchedulingMinimizeMakespan,
    ];

    assert_eq!(NativeHandlerV1::ALL.get(19..29), Some(expected.as_slice()));
}

#[test]
fn workflow_handlers_follow_scheduling_in_stable_order() {
    let expected = [
        NativeHandlerV1::WorkflowInitialStateAllowed,
        NativeHandlerV1::WorkflowTransitionAllowed,
        NativeHandlerV1::WorkflowSafetyInvariant,
        NativeHandlerV1::WorkflowBoundedReachability,
    ];

    assert_eq!(NativeHandlerV1::ALL.get(29..33), Some(expected.as_slice()));
}

#[test]
fn data_integrity_handlers_follow_workflow_in_stable_order() {
    let expected = [
        NativeHandlerV1::DataIntegrityUnique,
        NativeHandlerV1::DataIntegrityForeignKey,
        NativeHandlerV1::DataIntegrityCardinality,
        NativeHandlerV1::DataIntegrityValueRange,
        NativeHandlerV1::DataIntegrityConditionalRequired,
        NativeHandlerV1::DataIntegrityAggregateBalance,
        NativeHandlerV1::DataIntegrityMutuallyConsistent,
        NativeHandlerV1::DataIntegrityTemporalConsistency,
    ];

    assert_eq!(NativeHandlerV1::ALL.get(33..41), Some(expected.as_slice()));
}

#[test]
fn execution_kind_defaults_to_constraint_and_serializes_explicitly() {
    let source = serde_yml::to_string(&rule_fixture(
        AvailabilityV1::Implemented,
        RuleStrengthV1::Hard,
        Some(NativeHandlerV1::A11yTargetSize),
    ))
    .expect("serialize rule fixture");
    let source = source
        .lines()
        .filter(|line| !line.starts_with("execution_kind:"))
        .collect::<Vec<_>>()
        .join("\n");

    let omitted: RuleManifestV1 = serde_yml::from_str(&source).expect("default execution kind");
    assert_eq!(omitted.execution_kind, ExecutionKindV1::Constraint);
    assert_eq!(
        serde_json::to_value(omitted).expect("serialize defaulted rule")["execution_kind"],
        "constraint"
    );
}

#[test]
fn routing_truth_table_matches_constraint_and_objective_manifest_gates() {
    let availabilities = [
        AvailabilityV1::Implemented,
        AvailabilityV1::Experimental,
        AvailabilityV1::CapabilityUnavailable,
    ];
    let strengths = [
        RuleStrengthV1::Hard,
        RuleStrengthV1::Soft,
        RuleStrengthV1::Advisory,
    ];

    for execution_kind in [ExecutionKindV1::Constraint, ExecutionKindV1::Objective] {
        for availability in availabilities {
            for strength in strengths {
                for handler_present in [false, true] {
                    let executable = availability == AvailabilityV1::Implemented
                        && match execution_kind {
                            ExecutionKindV1::Constraint => strength == RuleStrengthV1::Hard,
                            ExecutionKindV1::Objective => true,
                        };
                    let handler = handler_present.then_some(NativeHandlerV1::A11yTargetSize);
                    let mut rule = rule_fixture(availability, strength, handler);
                    rule.execution_kind = execution_kind;
                    rule.conformance = executable.then(conformance_fixture);
                    if execution_kind == ExecutionKindV1::Objective {
                        rule.examples.invalid.expected_diagnostic = None;
                        if let Some(conformance) = &mut rule.conformance {
                            for vector in &mut conformance.invalid {
                                vector.expected_diagnostic = None;
                            }
                        }
                    }
                    let result = validate_rule_manifest(&rule);

                    assert_eq!(rule.is_executable(), executable);
                    assert_eq!(
                        result.is_ok(),
                        executable == handler_present,
                        "kind={execution_kind:?}, availability={availability:?}, strength={strength:?}, handler={handler_present}"
                    );
                    if executable && handler_present {
                        assert_eq!(result, Ok(ManifestRouteV1::Executable));
                    } else if !executable && !handler_present {
                        assert_eq!(result, Ok(ManifestRouteV1::CatalogOnly));
                    }
                }
            }
        }
    }
}

#[test]
fn objective_invalid_vectors_do_not_advertise_verification_diagnostics() {
    let mut objective = rule_fixture(
        AvailabilityV1::Implemented,
        RuleStrengthV1::Advisory,
        Some(NativeHandlerV1::RbacMinimumPrivilege),
    );
    objective.execution_kind = ExecutionKindV1::Objective;
    objective.conformance = Some(conformance_fixture());
    objective.examples.invalid.expected_diagnostic = None;
    objective.conformance.as_mut().expect("conformance").invalid[0].expected_diagnostic = None;
    assert_eq!(
        validate_rule_manifest(&objective),
        Ok(ManifestRouteV1::Executable)
    );

    let mut example_diagnostic = objective.clone();
    example_diagnostic.examples.invalid.expected_diagnostic = Some("not attributable".to_owned());
    assert!(validate_rule_manifest(&example_diagnostic).is_err());

    let mut vector_diagnostic = objective;
    vector_diagnostic
        .conformance
        .as_mut()
        .expect("conformance")
        .invalid[0]
        .expected_diagnostic = Some("not attributable".to_owned());
    assert!(validate_rule_manifest(&vector_diagnostic).is_err());
}

#[test]
fn objective_handlers_are_family_owned_and_have_empty_parameter_abis() {
    for (handler, family_id, profile_id) in [
        (NativeHandlerV1::RbacMinimumPrivilege, "policy", "nist_rbac"),
        (
            NativeHandlerV1::PlacementMinimizeSkew,
            "resource",
            "topology_placement",
        ),
    ] {
        let mut objective = rule_fixture(
            AvailabilityV1::Implemented,
            RuleStrengthV1::Advisory,
            Some(handler),
        );
        objective.execution_kind = ExecutionKindV1::Objective;
        objective.family = family_id.to_owned();
        objective.profile = profile_id.to_owned();
        objective.conformance = Some(conformance_fixture());
        objective.examples.invalid.expected_diagnostic = None;
        objective.conformance.as_mut().expect("conformance").invalid[0].expected_diagnostic = None;

        let mut bundle = bundle_fixture();
        bundle.families[0].id = family_id.to_owned();
        bundle.families[0].profiles[0].id = profile_id.to_owned();
        bundle.rules[0] = objective;
        assert_eq!(validate_manifest_bundle(&bundle), Ok(()));

        let mut unexpected_parameter = bundle.clone();
        unexpected_parameter.rules[0].parameters = vec![ParameterContractV1 {
            name: "unexpected".to_owned(),
            required: false,
            default: None,
            kind: ParameterKindV1::Boolean,
            minimum: None,
            maximum: None,
            values: Vec::new(),
            min_items: None,
            max_items: None,
            validator: None,
        }];
        assert!(validate_manifest_bundle(&unexpected_parameter).is_err());

        let mut wrong_family = bundle;
        wrong_family.families[0].id = "accessibility".to_owned();
        wrong_family.families[0].profiles[0].id = "wcag_geometry_color".to_owned();
        wrong_family.rules[0].family = "accessibility".to_owned();
        wrong_family.rules[0].profile = "wcag_geometry_color".to_owned();
        assert!(validate_manifest_bundle(&wrong_family).is_err());
    }
}

#[test]
fn implemented_hard_rules_require_valid_and_invalid_conformance_vectors() {
    let mut rule = rule_fixture(
        AvailabilityV1::Implemented,
        RuleStrengthV1::Hard,
        Some(NativeHandlerV1::A11yTargetSize),
    );
    rule.conformance = None;
    assert!(validate_rule_manifest(&rule).is_err());

    rule.conformance = Some(conformance_fixture());
    rule.conformance
        .as_mut()
        .expect("conformance")
        .valid
        .clear();
    assert!(validate_rule_manifest(&rule).is_err());

    rule.conformance = Some(conformance_fixture());
    rule.conformance
        .as_mut()
        .expect("conformance")
        .invalid
        .clear();
    assert!(validate_rule_manifest(&rule).is_err());
}

#[test]
fn conformance_vector_names_are_unique_across_both_outcomes() {
    let mut rule = rule_fixture(
        AvailabilityV1::Implemented,
        RuleStrengthV1::Hard,
        Some(NativeHandlerV1::A11yTargetSize),
    );
    let conformance = rule.conformance.as_mut().expect("conformance");
    conformance.invalid[0].name = conformance.valid[0].name.clone();

    assert!(validate_rule_manifest(&rule).is_err());
}

#[test]
fn parameter_defaults_bounds_and_enum_values_are_validated_statically() {
    let invalid_parameters = [
        ParameterContractV1 {
            name: "count".to_owned(),
            required: false,
            default: Some(json!(2)),
            kind: ParameterKindV1::Integer,
            minimum: Some(3),
            maximum: Some(1),
            values: Vec::new(),
            min_items: None,
            max_items: None,
            validator: None,
        },
        ParameterContractV1 {
            name: "mode".to_owned(),
            required: false,
            default: Some(json!("missing")),
            kind: ParameterKindV1::StringEnum,
            minimum: None,
            maximum: None,
            values: vec!["known".to_owned()],
            min_items: None,
            max_items: None,
            validator: None,
        },
        ParameterContractV1 {
            name: "roles".to_owned(),
            required: false,
            default: Some(json!(["one", 2])),
            kind: ParameterKindV1::StringArray,
            minimum: None,
            maximum: None,
            values: Vec::new(),
            min_items: Some(2),
            max_items: Some(1),
            validator: None,
        },
        ParameterContractV1 {
            name: "exception".to_owned(),
            required: false,
            default: Some(json!(false)),
            kind: ParameterKindV1::NativeObject,
            minimum: None,
            maximum: None,
            values: Vec::new(),
            min_items: None,
            max_items: None,
            validator: Some(NativeObjectValidatorV1::AccessibilityException),
        },
    ];

    for parameter in invalid_parameters {
        let mut rule = rule_fixture(
            AvailabilityV1::Implemented,
            RuleStrengthV1::Hard,
            Some(NativeHandlerV1::A11yTargetSize),
        );
        rule.parameters = vec![parameter];
        assert!(validate_rule_manifest(&rule).is_err());
    }

    let mut duplicate_names = rule_fixture(
        AvailabilityV1::Implemented,
        RuleStrengthV1::Hard,
        Some(NativeHandlerV1::A11yTargetSize),
    );
    let parameter = ParameterContractV1 {
        name: "minimum".to_owned(),
        required: true,
        default: None,
        kind: ParameterKindV1::Integer,
        minimum: Some(1),
        maximum: Some(10),
        values: Vec::new(),
        min_items: None,
        max_items: None,
        validator: None,
    };
    duplicate_names.parameters = vec![parameter.clone(), parameter];
    assert!(validate_rule_manifest(&duplicate_names).is_err());
}

#[test]
fn every_parameter_kind_accepts_a_valid_strict_contract() {
    let mut rule = rule_fixture(
        AvailabilityV1::Implemented,
        RuleStrengthV1::Hard,
        Some(NativeHandlerV1::A11yTargetSize),
    );
    rule.parameters = vec![
        ParameterContractV1 {
            name: "count".to_owned(),
            required: false,
            default: Some(json!(2)),
            kind: ParameterKindV1::Integer,
            minimum: Some(1),
            maximum: Some(3),
            values: Vec::new(),
            min_items: None,
            max_items: None,
            validator: None,
        },
        ParameterContractV1 {
            name: "enabled".to_owned(),
            required: false,
            default: Some(json!(true)),
            kind: ParameterKindV1::Boolean,
            minimum: None,
            maximum: None,
            values: Vec::new(),
            min_items: None,
            max_items: None,
            validator: None,
        },
        ParameterContractV1 {
            name: "label".to_owned(),
            required: true,
            default: None,
            kind: ParameterKindV1::String,
            minimum: None,
            maximum: None,
            values: Vec::new(),
            min_items: None,
            max_items: None,
            validator: None,
        },
        ParameterContractV1 {
            name: "mode".to_owned(),
            required: false,
            default: Some(json!("strict")),
            kind: ParameterKindV1::StringEnum,
            minimum: None,
            maximum: None,
            values: vec!["strict".to_owned(), "relaxed".to_owned()],
            min_items: None,
            max_items: None,
            validator: None,
        },
        ParameterContractV1 {
            name: "roles".to_owned(),
            required: false,
            default: Some(json!(["reader"])),
            kind: ParameterKindV1::StringArray,
            minimum: None,
            maximum: None,
            values: Vec::new(),
            min_items: Some(1),
            max_items: Some(2),
            validator: None,
        },
        ParameterContractV1 {
            name: "exception".to_owned(),
            required: false,
            default: Some(json!({"kind": "spacing", "applies": true})),
            kind: ParameterKindV1::NativeObject,
            minimum: None,
            maximum: None,
            values: Vec::new(),
            min_items: None,
            max_items: None,
            validator: Some(NativeObjectValidatorV1::AccessibilityException),
        },
    ];

    let yaml = serde_yml::to_string(&rule).expect("serialize strict rule fixture");
    let parsed: RuleManifestV1 = serde_yml::from_str(&yaml).expect("parse strict rule fixture");
    assert_eq!(parsed, rule);
    assert_eq!(
        validate_rule_manifest(&rule),
        Ok(ManifestRouteV1::Executable)
    );
}

#[test]
fn bundle_validation_rejects_duplicate_ids_handlers_and_missing_owners() {
    let mut duplicate_rule = bundle_fixture();
    let mut second = duplicate_rule.rules[0].clone();
    second.id = "demo.second".to_owned();
    duplicate_rule.rules.push(second.clone());
    assert!(validate_manifest_bundle(&duplicate_rule).is_err());

    second.handler = Some(NativeHandlerV1::A11yTextContrast);
    duplicate_rule.rules[1] = second.clone();
    duplicate_rule.rules.push(second);
    assert!(validate_manifest_bundle(&duplicate_rule).is_err());

    let mut missing_family = bundle_fixture();
    missing_family.rules[0].family = "missing".to_owned();
    assert!(validate_manifest_bundle(&missing_family).is_err());

    let mut missing_profile = bundle_fixture();
    missing_profile.rules[0].profile = "demo.missing".to_owned();
    assert!(validate_manifest_bundle(&missing_profile).is_err());

    let mut mismatched_profile_owner = bundle_fixture();
    let mut other_family = family_fixture();
    other_family.id = "other".to_owned();
    other_family.profiles[0].id = "other.default".to_owned();
    mismatched_profile_owner.families.push(other_family);
    mismatched_profile_owner.rules[0].family = "other".to_owned();
    assert!(validate_manifest_bundle(&mismatched_profile_owner).is_err());
}

#[test]
fn subject_cardinality_rejects_inverted_ranges() {
    let mut rule = rule_fixture(
        AvailabilityV1::Implemented,
        RuleStrengthV1::Hard,
        Some(NativeHandlerV1::A11yTargetSize),
    );
    rule.subjects.cardinality = SubjectCardinalityV1::Range {
        minimum: 2,
        maximum: 1,
    };
    assert!(validate_rule_manifest(&rule).is_err());
}
