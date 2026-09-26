use spur_jev::client::{JevTransport as _, MockTransport};
use spur_jev::wire::{
    Answer, JevRequest, JevResponse, Question, DEFAULT_MODEL, MAX_CHOICE_OPTIONS, MAX_SCORE_LEVELS,
};

#[test]
fn recorded_choice_answer_roundtrips() {
    let raw = serde_json::json!({
        "model": "jev-1.13.0",
        "answers": {
            "route_rule": {
                "type": "choice", "choice": "layout.containment",
                "probabilities": {"layout.containment": 1.0, "layout.non_overlap": 0.0},
                "confidence": 1.0
            },
            "data_complete": {"type": "noul", "noul": 0.95}
        },
        "usage": {"input_tokens": 829, "output_tokens": 136}
    });
    let parsed: JevResponse = serde_json::from_value(raw.clone()).expect("decode");
    assert_eq!(parsed.model, "jev-1.13.0");
    match &parsed.answers["route_rule"] {
        Answer::Choice {
            choice, confidence, ..
        } => {
            assert_eq!(choice, "layout.containment");
            assert!((*confidence - 1.0).abs() < 1e-9);
        }
        other => panic!("expected choice answer, got {other:?}"),
    }
    assert_eq!(serde_json::to_value(&parsed).expect("encode"), raw);
}

#[tokio::test]
async fn mock_transport_returns_canned_response() {
    let request: JevRequest = serde_json::from_value(serde_json::json!({
        "state": "Classify this request.",
        "model": "jev-latest",
        "questions": {
            "route_rule": {
                "type": "choice",
                "criteria": {"layout.containment": "Keep a child inside its parent."}
            }
        }
    }))
    .expect("request");
    let canned: JevResponse = serde_json::from_value(serde_json::json!({
        "model": "jev-1.13.0",
        "answers": {
            "route_rule": {
                "type": "choice",
                "choice": "layout.containment",
                "probabilities": {"layout.containment": 1.0},
                "confidence": 1.0
            }
        },
        "usage": {"input_tokens": 20, "output_tokens": 4}
    }))
    .expect("response");

    let serialized = serde_json::to_value(&request).expect("serialize request");
    assert_eq!(serialized["model"], DEFAULT_MODEL);
    assert!(serialized["questions"].get("route_rule").is_some());

    let transport = MockTransport::new(canned.clone());
    assert_eq!(transport.send(&request).await.expect("send"), canned);
}

#[test]
fn every_question_and_answer_kind_roundtrips() {
    let request_raw = serde_json::json!({
        "state": {"message": "Please review this incident."},
        "model": "jev-latest",
        "questions": {
            "actionable": {
                "type": "noul",
                "instructions": "Is the incident actionable?",
                "criteria": {"true": "Act now", "false": "No action needed"}
            },
            "route": {
                "type": "choice",
                "instructions": "Choose a route.",
                "criteria": {"close": "Close it", "review": "Review it"}
            },
            "urgency": {
                "type": "score",
                "instructions": "Rate urgency.",
                "criteria": ["Can wait", "Needs attention", "Act now"]
            }
        }
    });
    let request: JevRequest = serde_json::from_value(request_raw.clone()).expect("decode");
    assert_eq!(request.model, DEFAULT_MODEL);
    assert!(matches!(
        request.questions["actionable"],
        Question::Noul { .. }
    ));
    assert!(matches!(
        request.questions["route"],
        Question::Choice { .. }
    ));
    assert!(matches!(
        request.questions["urgency"],
        Question::Score { .. }
    ));
    assert_eq!(serde_json::to_value(request).expect("encode"), request_raw);

    let response_raw = serde_json::json!({
        "model": "jev-1.13.0",
        "answers": {
            "actionable": {"type": "noul", "noul": 0.75},
            "route": {
                "type": "choice",
                "choice": "review",
                "probabilities": {"close": 0.2, "review": 0.8},
                "confidence": 0.7
            },
            "urgency": {
                "type": "score",
                "score": 1.7,
                "legend": {"0": "Can wait", "1": "Needs attention", "2": "Act now"},
                "probabilities": {"0": 0.1, "1": 0.1, "2": 0.8},
                "confidence": 0.9
            }
        },
        "usage": {"input_tokens": 120, "output_tokens": 12}
    });
    let response: JevResponse =
        serde_json::from_value(response_raw.clone()).expect("decode response");
    assert!(matches!(
        response.answers["actionable"],
        Answer::Noul { .. }
    ));
    assert!(matches!(response.answers["route"], Answer::Choice { .. }));
    assert!(matches!(response.answers["urgency"], Answer::Score { .. }));
    assert_eq!(
        serde_json::to_value(response).expect("encode"),
        response_raw
    );

    assert_eq!(MAX_CHOICE_OPTIONS, 255);
    assert_eq!(MAX_SCORE_LEVELS, 10);
}
