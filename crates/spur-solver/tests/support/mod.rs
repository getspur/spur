#![allow(dead_code)]

use serde_json::json;
use spur_solver::rules::manifest_format::{
    AvailabilityV1, CatalogExampleV1, CatalogExamplesV1, ConformanceVectorV1, ConformanceVectorsV1,
    FamilyManifestV1, LlmEncodingV1, ManifestBundleV1, NativeHandlerV1, ProfileManifestV1,
    RuleManifestV1, RuleStrengthV1, SchemaVersionV1, SolverEncodingV1, SubjectCardinalityV1,
    SubjectContractV1,
};

pub fn family_fixture() -> FamilyManifestV1 {
    FamilyManifestV1 {
        schema_version: SchemaVersionV1,
        id: "demo".to_owned(),
        family_version: 1,
        summary: "Demo family".to_owned(),
        profiles: vec![ProfileManifestV1 {
            id: "demo.default".to_owned(),
            profile_version: 1,
            summary: "Demo profile".to_owned(),
        }],
    }
}

pub fn conformance_fixture() -> ConformanceVectorsV1 {
    ConformanceVectorsV1 {
        valid: vec![ConformanceVectorV1 {
            name: "accepts valid input".to_owned(),
            request: json!({"subjects": ["subject"]}),
            expected_diagnostic: None,
        }],
        invalid: vec![ConformanceVectorV1 {
            name: "rejects invalid input".to_owned(),
            request: json!({"subjects": []}),
            expected_diagnostic: Some("invalid demo".to_owned()),
        }],
    }
}

pub fn rule_fixture(
    availability: AvailabilityV1,
    strength: RuleStrengthV1,
    handler: Option<NativeHandlerV1>,
) -> RuleManifestV1 {
    let implemented_hard =
        availability == AvailabilityV1::Implemented && strength == RuleStrengthV1::Hard;
    RuleManifestV1 {
        schema_version: SchemaVersionV1,
        id: "demo.rule".to_owned(),
        rule_version: 1,
        family: "demo".to_owned(),
        profile: "demo.default".to_owned(),
        primitive: "demo_primitive".to_owned(),
        summary: "Demo rule".to_owned(),
        availability,
        availability_reason: (availability == AvailabilityV1::CapabilityUnavailable)
            .then(|| "Capability is unavailable".to_owned()),
        strength,
        authorities: Vec::new(),
        requires: Vec::new(),
        llm_encoding: LlmEncodingV1 {
            effectiveness: "Use for demos".to_owned(),
            problem_shapes: Vec::new(),
            encode_steps: Vec::new(),
            anti_patterns: Vec::new(),
            escalate_when: Vec::new(),
        },
        solver_encoding: SolverEncodingV1 {
            theory: "Bool".to_owned(),
            verification: "Verify the demo".to_owned(),
            synthesis: "Synthesize the demo".to_owned(),
            formula: Vec::new(),
        },
        subjects: SubjectContractV1 {
            cardinality: SubjectCardinalityV1::Exact { count: 1 },
        },
        parameters: Vec::new(),
        handler,
        examples: CatalogExamplesV1 {
            valid: CatalogExampleV1 {
                facts: json!({}),
                expectation: "accepted".to_owned(),
                expected_diagnostic: None,
            },
            invalid: CatalogExampleV1 {
                facts: json!({}),
                expectation: "rejected".to_owned(),
                expected_diagnostic: Some("invalid demo".to_owned()),
            },
        },
        conformance: implemented_hard.then(conformance_fixture),
    }
}

pub fn bundle_fixture() -> ManifestBundleV1 {
    let mut family = family_fixture();
    family.id = "accessibility".to_owned();
    family.profiles[0].id = "wcag_geometry_color".to_owned();
    let mut rule = rule_fixture(
        AvailabilityV1::Implemented,
        RuleStrengthV1::Hard,
        Some(NativeHandlerV1::A11yFocusNotObscured),
    );
    rule.family = family.id.clone();
    rule.profile = family.profiles[0].id.clone();

    ManifestBundleV1 {
        schema_version: SchemaVersionV1,
        families: vec![family],
        rules: vec![rule],
    }
}
