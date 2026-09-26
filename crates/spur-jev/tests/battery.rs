use serde_json::{json, Value};
use spur_jev::{
    battery::{generate_routing_battery, BatteryError},
    snapshot::{CatalogSnapshot, RuleCard},
    wire::{Question, MAX_CHOICE_OPTIONS},
};

fn snapshot() -> CatalogSnapshot {
    CatalogSnapshot {
        language_version: 1,
        rules: vec![
            RuleCard {
                rule_id: "layout.containment".into(),
                family: "design".into(),
                summary: "Keep one axis-aligned rectangle inside another.".into(),
            },
            RuleCard {
                rule_id: "scheduling.cumulative_capacity".into(),
                family: "scheduling".into(),
                summary: "Keep concurrent demand within capacity.".into(),
            },
        ],
    }
}

fn instructions(question: &Question) -> &Value {
    match question {
        Question::Noul { instructions, .. }
        | Question::Choice { instructions, .. }
        | Question::Score { instructions, .. } => {
            instructions.as_ref().expect("generated instructions")
        }
    }
}

#[test]
fn battery_is_deterministic_and_embeds_evidence() {
    let intent = "check the icon stays inside the panel";
    let data_a = json!({
        "parent": {"width": 320, "height": 200},
        "child": {"width": 44, "height": 44}
    });
    let data_b = json!({
        "child": {"height": 44, "width": 44},
        "parent": {"height": 200, "width": 320}
    });
    let mut reversed_snapshot = snapshot();
    reversed_snapshot.rules.reverse();

    let a = generate_routing_battery(intent, &data_a, &snapshot(), 7).expect("battery");
    let b = generate_routing_battery(intent, &data_b, &reversed_snapshot, 7).expect("battery");

    assert_eq!(
        serde_json::to_string(&a).expect("serialize first battery"),
        serde_json::to_string(&b).expect("serialize second battery")
    );
    assert_eq!(a.len(), 3);
    assert!(a.contains_key("route_rule"));
    assert!(a.contains_key("solve_mode"));
    assert!(a.contains_key("data_complete"));

    for question in a.values() {
        let evidence = instructions(question);
        assert_eq!(evidence["intent"], intent);
        assert_eq!(evidence["data"], data_a);
        assert_eq!(evidence["language_version"], 7);
        assert!(evidence["question"].is_string());
    }

    let Question::Choice { criteria, .. } = &a["route_rule"] else {
        panic!("route_rule must be a choice");
    };
    assert_eq!(criteria.len(), snapshot().rules.len());
    assert_eq!(criteria["layout.containment"]["family"], "design");
    assert_eq!(
        criteria["scheduling.cumulative_capacity"]["family"],
        "scheduling"
    );
}

#[test]
fn too_many_route_options_returns_typed_error() {
    let snapshot = CatalogSnapshot {
        language_version: 1,
        rules: (0..=MAX_CHOICE_OPTIONS)
            .map(|index| RuleCard {
                rule_id: format!("test.rule_{index:03}"),
                family: "test".into(),
                summary: format!("Test rule {index}"),
            })
            .collect(),
    };

    let error = generate_routing_battery("route this", &json!({}), &snapshot, 1)
        .expect_err("256 options must be rejected");

    assert_eq!(
        error,
        BatteryError::TooManyOptions {
            actual: MAX_CHOICE_OPTIONS + 1,
            maximum: MAX_CHOICE_OPTIONS,
        }
    );
}
