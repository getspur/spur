//! Regression suite for the `raw` JSON passed through MCP graph tools.
//! When this drifts, brain prompts that parse specific fields may break.

use std::sync::Arc;

use serde_json::Value;
use spur_pm::beads_crate::{AdapterConfig, BeadsCrateAdapter};
use spur_pm::graph_engine::{GraphEngine, GraphEngineConfig};
use spur_pm::test_workspace::TestBeadsWorkspace;

type IdMap = Vec<(String, String)>;

async fn canonical_engine() -> (GraphEngine, TestBeadsWorkspace, String, IdMap) {
    let mut ws = TestBeadsWorkspace::init();
    let top = ws.create_issue("Top task");
    let blocked = ws.create_issue("Blocked task");
    let parent = ws.create_issue("Closed parent");
    ws.close_issue(&parent);
    ws.add_dep(&top, &parent);
    ws.add_dep(&blocked, &top);

    let id_map = vec![
        (top.clone(), "<id-1>".to_string()),
        (blocked, "<id-2>".to_string()),
        (parent, "<id-3>".to_string()),
    ];
    let beads = Arc::new(
        BeadsCrateAdapter::open(ws.path(), AdapterConfig::default())
            .await
            .expect("open beads crate adapter"),
    );
    let engine = GraphEngine::new(beads, GraphEngineConfig::default());
    (engine, ws, top, id_map)
}

fn assert_snapshot(actual: &Value, name: &str, ids: &IdMap) {
    let path = format!("tests/snapshots/{name}.json");
    let mut actual_norm = actual.clone();
    normalize(&mut actual_norm, ids);

    if std::env::var("UPDATE_SNAPSHOTS").is_ok() {
        std::fs::create_dir_all("tests/snapshots").expect("create snapshot directory");
        let pretty = serde_json::to_string_pretty(&actual_norm).expect("pretty snapshot json");
        std::fs::write(&path, format!("{pretty}\n")).expect("write snapshot");
        return;
    }

    let expected_str = std::fs::read_to_string(&path).expect("snapshot file present");
    let mut expected: Value = serde_json::from_str(&expected_str).expect("parse snapshot json");
    normalize(&mut expected, ids);
    assert_eq!(expected, actual_norm, "snapshot drift in {name}");
}

fn normalize(v: &mut Value, ids: &IdMap) {
    normalize_with_key(v, ids, None);
}

fn normalize_with_key(v: &mut Value, ids: &IdMap, parent_key: Option<&str>) {
    match v {
        Value::Object(obj) => {
            for (key, value) in obj.iter_mut() {
                if is_volatile_field(key) {
                    *value = Value::String("<normalized>".to_string());
                } else {
                    normalize_with_key(value, ids, Some(key));
                }
            }
        }
        Value::Array(arr) => {
            for value in arr.iter_mut() {
                normalize_with_key(value, ids, parent_key);
            }
            if should_sort_array(parent_key) {
                arr.sort_by_key(stable_json);
            }
        }
        Value::String(s) => {
            for (id, replacement) in ids {
                *s = s.replace(id, replacement);
            }
            if looks_like_iso8601_timestamp(s) {
                *s = "<normalized>".to_string();
            }
        }
        Value::Number(number) if number.is_f64() => {
            if let Some(f) = number.as_f64() {
                let rounded = (f * 1_000_000_000_000.0).round() / 1_000_000_000_000.0;
                if let Some(number) = serde_json::Number::from_f64(rounded) {
                    *v = Value::Number(number);
                }
            }
        }
        _ => {}
    }
}

fn should_sort_array(parent_key: Option<&str>) -> bool {
    matches!(
        parent_key,
        Some(
            "nodes"
                | "edges"
                | "Cores"
                | "Hubs"
                | "Authorities"
                | "Articulation"
                | "Orphans"
                | "Cycles"
                | "issue_ids"
                | "unblocks_ids"
                | "blocked_by"
        )
    )
}

fn stable_json(value: &Value) -> String {
    serde_json::to_string(value).expect("normalized value serializes")
}

fn is_volatile_field(key: &str) -> bool {
    matches!(
        key,
        "generated_at" | "created_at" | "updated_at" | "data_hash"
    )
}

fn looks_like_iso8601_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 20
        && b.get(4) == Some(&b'-')
        && b.get(7) == Some(&b'-')
        && b.get(10) == Some(&b'T')
        && (s.ends_with('Z') || s.contains('+'))
}

#[tokio::test(flavor = "current_thread")]
async fn triage_snapshot() {
    let (engine, _ws, _top, ids) = canonical_engine().await;
    let report = engine.triage(None).await.expect("triage report");
    assert_snapshot(&report.raw, "triage", &ids);
}

#[tokio::test(flavor = "current_thread")]
async fn plan_snapshot() {
    let (engine, _ws, _top, ids) = canonical_engine().await;
    let report = engine.plan(None).await.expect("plan report");
    assert_snapshot(&report.raw, "plan", &ids);
}

#[tokio::test(flavor = "current_thread")]
async fn insights_snapshot() {
    let (engine, _ws, _top, ids) = canonical_engine().await;
    let report = engine.insights(None).await.expect("insights report");
    assert_snapshot(&report.raw, "insights", &ids);
}

#[tokio::test(flavor = "current_thread")]
async fn alerts_snapshot() {
    let (engine, _ws, _top, ids) = canonical_engine().await;
    let report = engine.alerts().await.expect("alerts report");
    assert_snapshot(&report.raw, "alerts", &ids);
}

#[tokio::test(flavor = "current_thread")]
async fn subgraph_snapshot() {
    let (engine, _ws, top, ids) = canonical_engine().await;
    let report = engine
        .subgraph(&top, Some(1), Some("json"))
        .await
        .expect("subgraph report");
    assert_snapshot(&report.raw, "subgraph", &ids);
}
