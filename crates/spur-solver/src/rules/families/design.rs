//! Geometry rules for UI layout and graphic design.

pub mod compile;
pub mod scene;

use std::sync::LazyLock;

use serde_json::json;

use crate::rules::catalog::{
    LlmEncoding, RuleAuthority, RuleDefinition, RuleExample, RuleExamples, RuleFamily,
    RuleGuidance, RuleProfile, RuleRegistry, SolverEncoding,
};

static BUILTIN_REGISTRY: LazyLock<RuleRegistry> = LazyLock::new(|| {
    RuleRegistry::new(
        1,
        vec![RuleFamily::new(
            "design",
            "Mathematically enforceable UI layout and graphic-design rules.",
            ["geometric_integrity", "layout_capacity"],
        )],
        vec![
            RuleProfile::new(
                "geometric_integrity",
                "design",
                "Containment, separation, and media-shape invariants.",
                [
                    "layout.containment",
                    "layout.non_overlap",
                    "media.aspect_ratio",
                ],
            ),
            RuleProfile::new(
                "layout_capacity",
                "design",
                "One-dimensional capacity constraints over declared extents.",
                ["layout.axis_capacity"],
            ),
        ],
        vec![
            axis_capacity_rule(),
            containment_rule(),
            non_overlap_rule(),
            aspect_ratio_rule(),
        ],
    )
    .unwrap_or_else(|error| panic!("built-in design rule registry is invalid: {error}"))
});

/// Returns the validated built-in multi-family registry seeded with design.
#[must_use]
pub fn builtin_registry() -> &'static RuleRegistry {
    &BUILTIN_REGISTRY
}

fn axis_capacity_rule() -> RuleDefinition {
    RuleDefinition::new(
        "layout.axis_capacity",
        "design",
        "layout_capacity",
        "axis_capacity",
        "Fit declared item extents, gaps, and insets inside one available axis extent.",
    )
    .with_guidance(RuleGuidance::implemented_hard(
        vec![css_visual_formatting_authority()],
        [
            "container.rect",
            "item.rect[]",
            "axis",
            "gap",
            "inset_start",
            "inset_end",
        ],
        LlmEncoding::new(
            "high",
            [
                "declared items must fit one horizontal or vertical extent",
                "fixed columns or rows must fit their available axis",
            ],
            [
                "Select the declared horizontal or vertical axis",
                "Read the container extent and each item extent on that axis",
                "Add one gap between adjacent items and both insets",
                "Require total used extent to be at most the available extent",
            ],
            [
                "Do not infer item membership from visual proximity",
                "Do not mix coordinate units in one binding",
            ],
            ["Wrapping, flexing, or weighted tracks require separate rules"],
        ),
        SolverEncoding::new(
            "QF_LIA",
            "assert the capacity inequality over a complete model",
            "assert the capacity inequality over explicitly bounded unknowns",
            [
                "sum(item extents) + gap * (item count - 1) + inset_start + inset_end <= available extent",
            ],
        ),
        RuleExamples::new(
            RuleExample::new(
                json!({
                    "available_extent": 100,
                    "item_extents": [30, 30],
                    "gap": 20,
                    "inset_start": 10,
                    "inset_end": 10
                }),
                "pass",
                None::<String>,
            ),
            RuleExample::new(
                json!({
                    "available_extent": 100,
                    "item_extents": [30, 31],
                    "gap": 20,
                    "inset_start": 10,
                    "inset_end": 10
                }),
                "counterexample",
                Some("design.axis_capacity_exceeded"),
            ),
        ),
    ))
}

fn containment_rule() -> RuleDefinition {
    RuleDefinition::new(
        "layout.containment",
        "design",
        "geometric_integrity",
        "inside",
        "Keep one axis-aligned rectangle inside another with optional padding.",
    )
    .with_guidance(RuleGuidance::implemented_hard(
        vec![css_visual_formatting_authority()],
        ["child.rect", "parent.rect", "padding"],
        LlmEncoding::new(
            "high",
            [
                "child must remain inside parent",
                "content must remain inside viewport",
            ],
            [
                "Resolve child and parent rectangle edges",
                "Apply non-negative padding to all four boundaries",
                "Emit four linear inequalities",
            ],
            [
                "Do not apply containment to intentional overlays",
                "Do not infer a parent binding from visual proximity",
            ],
            ["Rotated geometry requires transformed bounds"],
        ),
        SolverEncoding::new(
            "QF_LIA",
            "assert all four containment inequalities over a complete model",
            "assert all four containment inequalities over bounded unknowns",
            [
                "parent.left + padding <= child.left",
                "parent.top + padding <= child.top",
                "child.right + padding <= parent.right",
                "child.bottom + padding <= parent.bottom",
            ],
        ),
        RuleExamples::new(
            RuleExample::new(
                json!({
                    "parent": {"x": 0, "y": 0, "width": 320, "height": 200},
                    "child": {"x": 16, "y": 16, "width": 44, "height": 44},
                    "padding": 0
                }),
                "pass",
                None::<String>,
            ),
            RuleExample::new(
                json!({
                    "parent": {"x": 0, "y": 0, "width": 320, "height": 200},
                    "child": {"x": 300, "y": 16, "width": 44, "height": 44},
                    "padding": 0
                }),
                "counterexample",
                Some("design.outside_parent"),
            ),
        ),
    ))
}

fn non_overlap_rule() -> RuleDefinition {
    RuleDefinition::new(
        "layout.non_overlap",
        "design",
        "geometric_integrity",
        "disjoint",
        "Separate two axis-aligned rectangles by an optional minimum gap.",
    )
    .with_guidance(RuleGuidance::implemented_hard(
        vec![css_visual_formatting_authority()],
        ["first.rect", "second.rect", "minimum_gap"],
        LlmEncoding::new(
            "high",
            ["siblings must not overlap", "reserved regions must remain separate"],
            [
                "Resolve both rectangle edge sets",
                "Apply the non-negative minimum gap",
                "Emit left, right, above, or below as a four-way disjunction",
            ],
            [
                "Do not apply to intentional overlays or badges",
                "Do not require separation for hidden elements",
            ],
            ["Rotated or curved shapes require a geometry preprocessor"],
        ),
        SolverEncoding::new(
            "QF_LIA",
            "assert at least one separating relation over a complete model",
            "assert at least one separating relation over bounded unknowns",
            [
                "first.right + minimum_gap <= second.left OR second.right + minimum_gap <= first.left OR first.bottom + minimum_gap <= second.top OR second.bottom + minimum_gap <= first.top",
            ],
        ),
        RuleExamples::new(
            RuleExample::new(
                json!({
                    "first": {"x": 0, "y": 0, "width": 100, "height": 100},
                    "second": {"x": 124, "y": 0, "width": 100, "height": 100},
                    "minimum_gap": 24
                }),
                "pass",
                None::<String>,
            ),
            RuleExample::new(
                json!({
                    "first": {"x": 0, "y": 0, "width": 100, "height": 100},
                    "second": {"x": 80, "y": 0, "width": 100, "height": 100},
                    "minimum_gap": 0
                }),
                "counterexample",
                Some("design.overlap"),
            ),
        ),
    ))
}

fn aspect_ratio_rule() -> RuleDefinition {
    RuleDefinition::new(
        "media.aspect_ratio",
        "design",
        "geometric_integrity",
        "aspect_ratio",
        "Preserve a source aspect ratio without division.",
    )
    .with_guidance(RuleGuidance::implemented_hard(
        vec![RuleAuthority::new(
            "css_spec",
            "CSS Box Sizing Module Level 4 - Aspect Ratios",
            "https://www.w3.org/TR/css-sizing-4/#aspect-ratio",
        )],
        ["render.rect", "source.width", "source.height"],
        LlmEncoding::new(
            "high",
            [
                "media must preserve intrinsic proportions",
                "preview must fit without distortion",
            ],
            [
                "Require positive source dimensions",
                "Cross-multiply source and rendered dimensions",
                "Avoid integer or real division",
            ],
            ["Do not use rounded decimal ratios as equality constraints"],
            ["Cropped media requires a separate cover-policy rule"],
        ),
        SolverEncoding::new(
            "QF_NIA",
            "assert equal cross products over a complete model",
            "assert equal cross products over bounded unknowns",
            ["render.width * source.height = render.height * source.width"],
        ),
        RuleExamples::new(
            RuleExample::new(
                json!({
                    "source": {"width": 16, "height": 9},
                    "render": {"width": 320, "height": 180}
                }),
                "pass",
                None::<String>,
            ),
            RuleExample::new(
                json!({
                    "source": {"width": 16, "height": 9},
                    "render": {"width": 320, "height": 200}
                }),
                "counterexample",
                Some("design.aspect_ratio_mismatch"),
            ),
        ),
    ))
}

fn css_visual_formatting_authority() -> RuleAuthority {
    RuleAuthority::new(
        "css_spec",
        "CSS 2.2 Visual Formatting Model",
        "https://www.w3.org/TR/CSS22/visuren.html",
    )
}
