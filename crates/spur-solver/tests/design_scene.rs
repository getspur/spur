use std::collections::BTreeMap;

use serde_json::json;
use spur_solver::rules::families::design::scene::{
    DesignField, DesignNode, DesignRect, DesignScene, DesignSceneError, DesignSize, DesignUnknown,
    MAX_DESIGN_NODES, MAX_DESIGN_UNKNOWNS,
};

fn node(parent: Option<&str>, x: Option<i64>, width: Option<i64>) -> DesignNode {
    DesignNode {
        parent: parent.map(str::to_owned),
        rect: DesignRect {
            x,
            y: Some(0),
            width,
            height: Some(40),
        },
    }
}

fn scene(nodes: impl IntoIterator<Item = (impl Into<String>, DesignNode)>) -> DesignScene {
    DesignScene {
        viewport: DesignSize {
            width: 390,
            height: 844,
        },
        nodes: nodes
            .into_iter()
            .map(|(id, node)| (id.into(), node))
            .collect(),
    }
}

#[test]
fn concrete_scene_deserializes_and_validates() {
    let scene: DesignScene = serde_json::from_value(json!({
        "viewport": {"width": 390, "height": 844},
        "nodes": {
            "panel": {"rect": {"x": 0, "y": 0, "width": 390, "height": 844}},
            "button": {
                "parent": "panel",
                "rect": {"x": 320, "y": 780, "width": 44, "height": 44}
            }
        }
    }))
    .expect("typed scene");

    scene.validate(&[]).expect("valid concrete scene");
    assert_eq!(scene.nodes["button"].rect.width, Some(44));
}

#[test]
fn omitted_geometry_is_valid_only_when_declared_as_one_bounded_unknown() {
    let unknown_scene = scene([
        ("panel", node(None, Some(0), Some(390))),
        ("button", node(Some("panel"), None, Some(44))),
    ]);
    let unknown = DesignUnknown {
        node: "button".to_owned(),
        field: DesignField::X,
        min: 0,
        max: 346,
    };

    assert_eq!(
        unknown_scene.validate(&[]),
        Err(DesignSceneError::MissingGeometry {
            node: "button".to_owned(),
            field: DesignField::X,
        })
    );
    unknown_scene
        .validate(std::slice::from_ref(&unknown))
        .expect("covered unknown");

    let concrete_unknown = scene([
        ("panel", node(None, Some(0), Some(390))),
        ("button", node(Some("panel"), Some(10), Some(44))),
    ]);
    assert_eq!(
        concrete_unknown.validate(&[unknown]),
        Err(DesignSceneError::UnknownHasConcreteValue {
            node: "button".to_owned(),
            field: DesignField::X,
        })
    );
}

#[test]
fn dimensions_and_unknown_ranges_are_validated_before_compilation() {
    let bad_viewport = DesignScene {
        viewport: DesignSize {
            width: 0,
            height: 844,
        },
        nodes: BTreeMap::new(),
    };
    assert_eq!(
        bad_viewport.validate(&[]),
        Err(DesignSceneError::NonPositiveViewport {
            width: 0,
            height: 844,
        })
    );

    let bad_width = scene([("item", node(None, Some(0), Some(0)))]);
    assert_eq!(
        bad_width.validate(&[]),
        Err(DesignSceneError::NonPositiveDimension {
            node: "item".to_owned(),
            field: DesignField::Width,
            value: 0,
        })
    );

    let unknown_width = scene([("item", node(None, Some(0), None))]);
    assert_eq!(
        unknown_width.validate(&[DesignUnknown {
            node: "item".to_owned(),
            field: DesignField::Width,
            min: 0,
            max: 100,
        }]),
        Err(DesignSceneError::NonPositiveDimensionRange {
            node: "item".to_owned(),
            field: DesignField::Width,
            min: 0,
        })
    );
    assert_eq!(
        unknown_width.validate(&[DesignUnknown {
            node: "item".to_owned(),
            field: DesignField::Width,
            min: 100,
            max: 10,
        }]),
        Err(DesignSceneError::InvalidUnknownRange {
            node: "item".to_owned(),
            field: DesignField::Width,
            min: 100,
            max: 10,
        })
    );
}

#[test]
fn parent_references_must_resolve_and_form_an_acyclic_forest() {
    let missing = scene([("child", node(Some("missing"), Some(0), Some(40)))]);
    assert_eq!(
        missing.validate(&[]),
        Err(DesignSceneError::UnknownParent {
            node: "child".to_owned(),
            parent: "missing".to_owned(),
        })
    );

    let cycle = scene([
        ("a", node(Some("b"), Some(0), Some(40))),
        ("b", node(Some("a"), Some(0), Some(40))),
    ]);
    assert_eq!(
        cycle.validate(&[]),
        Err(DesignSceneError::ParentCycle {
            node: "a".to_owned(),
        })
    );
}

#[test]
fn duplicate_unknown_paths_and_backend_budget_overflow_are_rejected() {
    let one_unknown_scene = scene([("item", node(None, None, Some(40)))]);
    let unknown = DesignUnknown {
        node: "item".to_owned(),
        field: DesignField::X,
        min: 0,
        max: 10,
    };
    assert_eq!(
        one_unknown_scene.validate(&[unknown.clone(), unknown]),
        Err(DesignSceneError::DuplicateUnknown {
            node: "item".to_owned(),
            field: DesignField::X,
        })
    );

    let oversized = scene((0..=MAX_DESIGN_NODES).map(|index| {
        (
            format!("node_{index}"),
            node(None, Some(index as i64), Some(1)),
        )
    }));
    assert_eq!(
        oversized.validate(&[]),
        Err(DesignSceneError::TooManyNodes {
            count: MAX_DESIGN_NODES + 1,
            max: MAX_DESIGN_NODES,
        })
    );

    let unknowns = (0..=MAX_DESIGN_UNKNOWNS)
        .map(|index| DesignUnknown {
            node: "item".to_owned(),
            field: DesignField::X,
            min: index as i64,
            max: index as i64,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        one_unknown_scene.validate(&unknowns),
        Err(DesignSceneError::TooManyUnknowns {
            count: MAX_DESIGN_UNKNOWNS + 1,
            max: MAX_DESIGN_UNKNOWNS,
        })
    );
}
