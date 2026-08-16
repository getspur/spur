//! Standard-backed accessibility rules over caller-supplied UI facts.

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
            "accessibility",
            "WCAG-backed geometry and contrast rules over normalized UI facts.",
            ["wcag_geometry_color"],
        )],
        vec![RuleProfile::new(
            "wcag_geometry_color",
            "accessibility",
            "Target, focus visibility, reflow, and text contrast constraints.",
            [
                "a11y.focus_not_obscured",
                "a11y.reflow",
                "a11y.target_size",
                "a11y.text_contrast",
            ],
        )],
        vec![
            focus_not_obscured_rule(),
            reflow_rule(),
            target_size_rule(),
            text_contrast_rule(),
        ],
    )
    .unwrap_or_else(|error| panic!("built-in accessibility registry is invalid: {error}"))
});

/// Returns the validated accessibility catalog contribution.
#[must_use]
pub fn builtin_registry() -> &'static RuleRegistry {
    &BUILTIN_REGISTRY
}

fn target_size_rule() -> RuleDefinition {
    rule(
        "a11y.target_size",
        "minimum_target_size",
        "Require a target to meet its minimum width and height unless a typed exception applies.",
        ["target.rect", "exception.kind", "exception.evidence"],
        [
            "exception OR target.width >= minimum_width",
            "exception OR target.height >= minimum_height",
        ],
        json!({"width": 24, "height": 24}),
        json!({"width": 23, "height": 24}),
    )
}

fn focus_not_obscured_rule() -> RuleDefinition {
    rule(
        "a11y.focus_not_obscured",
        "not_fully_contained",
        "Require a focused rectangle not to be fully hidden by one supplied obscurer.",
        ["focused.rect", "obscurer.rect"],
        ["NOT obscurer.contains(focused)"],
        json!({"focused": [0, 0, 24, 24], "obscurer": [12, 0, 24, 24]}),
        json!({"focused": [0, 0, 24, 24], "obscurer": [0, 0, 24, 24]}),
    )
}

fn reflow_rule() -> RuleDefinition {
    rule(
        "a11y.reflow",
        "viewport_fit",
        "Require content width to fit the supplied reflow viewport unless a typed exception applies.",
        ["content.rect", "viewport.width", "exception.kind", "exception.evidence"],
        ["exception OR content.width <= viewport.width"],
        json!({"viewport_width": 320, "content_width": 320}),
        json!({"viewport_width": 320, "content_width": 321}),
    )
}

fn text_contrast_rule() -> RuleDefinition {
    rule(
        "a11y.text_contrast",
        "contrast_ratio",
        "Require normalized foreground and background luminance to meet a minimum contrast ratio.",
        [
            "foreground_luminance",
            "background_luminance",
            "minimum_ratio_hundredths",
        ],
        [
            "(lighter + 0.05) * 100 >= (darker + 0.05) * minimum_ratio_hundredths",
            "relative luminance is normalized upstream to 0..=100000",
        ],
        json!({"foreground_luminance": 17500, "background_luminance": 0, "ratio": 450}),
        json!({"foreground_luminance": 17499, "background_luminance": 0, "ratio": 450}),
    )
}

fn rule(
    id: &str,
    primitive: &str,
    summary: &str,
    requires: impl IntoIterator<Item = &'static str>,
    formula: impl IntoIterator<Item = &'static str>,
    valid: serde_json::Value,
    invalid: serde_json::Value,
) -> RuleDefinition {
    RuleDefinition::new(
        id,
        "accessibility",
        "wcag_geometry_color",
        primitive,
        summary,
    )
    .with_guidance(RuleGuidance::implemented_hard(
        vec![wcag_authority()],
        requires,
        LlmEncoding::new(
            "high",
            [summary],
            [
                "Normalize caller facts",
                "Bind explicit subjects",
                "Compile the hard predicate",
            ],
            [
                "Do not infer facts from pixels inside the solver",
                "Do not treat an evidence string as solver-proven evidence",
            ],
            ["Escalate unions of obscurers or transformed geometry to a geometry preprocessor"],
        ),
        SolverEncoding::new(
            "QF_LIA",
            "assert the predicate over complete normalized facts",
            "assert the predicate over explicitly bounded numeric unknowns",
            formula,
        ),
        RuleExamples::new(
            RuleExample::new(valid, "pass", None::<String>),
            RuleExample::new(invalid, "counterexample", Some(format!("{id}.violation"))),
        ),
    ))
}

fn wcag_authority() -> RuleAuthority {
    RuleAuthority::new(
        "w3c_recommendation",
        "Web Content Accessibility Guidelines (WCAG) 2.2",
        "https://www.w3.org/TR/WCAG22/",
    )
}
