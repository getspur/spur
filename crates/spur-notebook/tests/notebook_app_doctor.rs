/// Tests for the `notebook_app_doctor` MCP tool (T5).
///
/// TDD: this file is the failing-test commit.  Each test is named after the
/// check it exercises.  The tool implementation lives in
/// `crates/spur-notebook/src/mcp/tools/notebook_app_doctor.rs`.
use std::{fs, sync::Arc, time::Duration};

use rmcp::model::CallToolResult;
use serde_json::{json, Value};
use spur_notebook::mcp::{
    bridge::{BridgeError, BridgeRequestFuture, BridgeRequester},
    tools::{self, notebook_app_doctor},
    ServerDeps,
};

// ── NullBridge (same pattern used by siblings) ────────────────────────────────

struct NullBridge;

impl BridgeRequester for NullBridge {
    fn listener_registered(&self) -> bool {
        false
    }
    fn window_alive(&self) -> bool {
        false
    }
    fn notebook_open(&self) -> bool {
        false
    }
    fn request<'a>(
        &'a self,
        method: &'static str,
        _params: Value,
        _timeout: Duration,
    ) -> BridgeRequestFuture<'a> {
        Box::pin(async move {
            Err(BridgeError::Handler {
                code: "unexpected_bridge_call".to_string(),
                message: format!("unexpected bridge call to {method}"),
            })
        })
    }
}

fn deps() -> ServerDeps {
    ServerDeps::from_bridge(Arc::new(NullBridge))
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Minimal valid spur-app.json content.
fn minimal_manifest(entry_notebook: &str) -> String {
    serde_json::to_string(&json!({
        "schema": "spur.app/v1",
        "name": "Test App",
        "entry_notebook": entry_notebook,
        "open_mode": "app",
        "runtime": { "jute_min": "0.1.0", "features": [] }
    }))
    .unwrap()
}

/// Minimal valid notebook JSON.
fn minimal_notebook() -> &'static str {
    r#"{"cells":[],"metadata":{},"nbformat":4,"nbformat_minor":5}"#
}

/// Extract structured content from a doctor result.
fn findings(result: serde_json::Value) -> Vec<Value> {
    result["findings"].as_array().cloned().unwrap_or_default()
}

fn has_fail(result: &Value) -> bool {
    result["ok"].as_bool() == Some(false)
}

fn has_check_level(result: &Value, check: &str, level: &str) -> bool {
    findings(result.clone())
        .iter()
        .any(|f| f["check"] == check && f["level"] == level)
}

// ── check 1: manifest parses ──────────────────────────────────────────────────

#[tokio::test]
async fn doctor_check1_pass_on_valid_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(root.join("spur-app.json"), minimal_manifest("app.ipynb")).expect("write manifest");
    fs::write(root.join("app.ipynb"), minimal_notebook()).expect("write notebook");

    let result = notebook_app_doctor::call(&deps(), json!({ "path": root.to_string_lossy() }))
        .await
        .expect("doctor call succeeds");

    let body = structured(result);
    assert!(
        body["ok"].as_bool().unwrap_or(false),
        "expected ok=true for valid app: {body}"
    );
    assert!(
        has_check_level(&body, "manifest", "pass"),
        "expected manifest:pass: {body}"
    );
}

#[tokio::test]
async fn doctor_check1_fail_missing_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");

    let result =
        notebook_app_doctor::call(&deps(), json!({ "path": dir.path().to_string_lossy() }))
            .await
            .expect("doctor call succeeds");

    let body = structured(result);
    assert!(
        has_fail(&body),
        "expected ok=false when manifest absent: {body}"
    );
    assert!(
        has_check_level(&body, "manifest", "fail"),
        "expected manifest:fail: {body}"
    );
}

#[tokio::test]
async fn doctor_check1_fail_wrong_schema() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(
        root.join("spur-app.json"),
        r#"{"schema":"spur.app/v0","name":"X","entry_notebook":"app.ipynb","open_mode":"app","runtime":{"jute_min":"0.1.0","features":[]}}"#,
    )
    .expect("write manifest");
    fs::write(root.join("app.ipynb"), minimal_notebook()).expect("write notebook");

    let result = notebook_app_doctor::call(&deps(), json!({ "path": root.to_string_lossy() }))
        .await
        .expect("doctor call succeeds");

    let body = structured(result);
    assert!(
        has_fail(&body),
        "expected ok=false for wrong schema: {body}"
    );
    assert!(has_check_level(&body, "manifest", "fail"), "{body}");
}

#[tokio::test]
async fn doctor_check1_fail_missing_entry_notebook() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(root.join("spur-app.json"), minimal_manifest("app.ipynb")).expect("write manifest");
    // intentionally do NOT write app.ipynb

    let result = notebook_app_doctor::call(&deps(), json!({ "path": root.to_string_lossy() }))
        .await
        .expect("doctor call succeeds");

    let body = structured(result);
    assert!(
        has_fail(&body),
        "expected ok=false for missing entry: {body}"
    );
    assert!(has_check_level(&body, "manifest", "fail"), "{body}");
}

// ── check 2: known capabilities ───────────────────────────────────────────────

#[tokio::test]
async fn doctor_check2_pass_known_capabilities() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(
        root.join("spur-app.json"),
        serde_json::to_string(&json!({
            "schema": "spur.app/v1",
            "name": "Cap App",
            "entry_notebook": "app.ipynb",
            "open_mode": "app",
            "runtime": { "jute_min": "0.1.0", "features": [] },
            "capabilities": {
                "active_output_scripts": true,
                "canvas_capture": true
            }
        }))
        .unwrap(),
    )
    .expect("write manifest");
    fs::write(root.join("app.ipynb"), minimal_notebook()).expect("write notebook");

    let result = notebook_app_doctor::call(&deps(), json!({ "path": root.to_string_lossy() }))
        .await
        .expect("doctor call succeeds");

    let body = structured(result);
    // capability checks should be pass-level for known keys
    assert!(
        has_check_level(&body, "capability:active_output_scripts", "pass"),
        "expected capability pass: {body}"
    );
}

#[tokio::test]
async fn doctor_check2_fail_unknown_capability() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    // Write raw JSON with an unknown capability key to bypass Rust serde
    fs::write(
        root.join("spur-app.json"),
        r#"{
            "schema": "spur.app/v1",
            "name": "Bad App",
            "entry_notebook": "app.ipynb",
            "open_mode": "app",
            "runtime": { "jute_min": "0.1.0", "features": [] },
            "capabilities": { "unknown_future_capability": true }
        }"#,
    )
    .expect("write manifest");
    fs::write(root.join("app.ipynb"), minimal_notebook()).expect("write notebook");

    let result = notebook_app_doctor::call(&deps(), json!({ "path": root.to_string_lossy() }))
        .await
        .expect("doctor call succeeds");

    let body = structured(result);
    assert!(
        has_fail(&body),
        "expected ok=false for unknown capability: {body}"
    );
    assert!(
        has_check_level(&body, "capability:unknown", "fail"),
        "expected capability:unknown fail: {body}"
    );
}

// ── check 3: ports declared as DAG sources ────────────────────────────────────

#[tokio::test]
async fn doctor_check3_pass_port_exists_as_dag_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    // Notebook with a cell whose dag metadata has the declared port
    let notebook = json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {},
        "cells": [{
            "cell_type": "code",
            "source": [],
            "metadata": {
                "spur": {
                    "dag": {
                        "source": {
                            "kind": "canvas-capture",
                            "port": "my-port"
                        }
                    }
                }
            },
            "outputs": [],
            "execution_count": null
        }]
    });

    fs::write(
        root.join("spur-app.json"),
        serde_json::to_string(&json!({
            "schema": "spur.app/v1",
            "name": "Port App",
            "entry_notebook": "app.ipynb",
            "open_mode": "app",
            "runtime": { "jute_min": "0.1.0", "features": [] },
            "capabilities": {
                "ports": { "read": ["my-port"], "write": [] }
            }
        }))
        .unwrap(),
    )
    .expect("write manifest");
    fs::write(
        root.join("app.ipynb"),
        serde_json::to_string(&notebook).unwrap(),
    )
    .expect("write notebook");

    let result = notebook_app_doctor::call(&deps(), json!({ "path": root.to_string_lossy() }))
        .await
        .expect("doctor call succeeds");

    let body = structured(result);
    assert!(
        has_check_level(&body, "port:my-port", "pass"),
        "expected port:my-port pass: {body}"
    );
}

#[tokio::test]
async fn doctor_check3_fail_undeclared_port() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    // Notebook with NO dag source for the declared port
    let notebook = json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {},
        "cells": []
    });

    fs::write(
        root.join("spur-app.json"),
        serde_json::to_string(&json!({
            "schema": "spur.app/v1",
            "name": "Port App",
            "entry_notebook": "app.ipynb",
            "open_mode": "app",
            "runtime": { "jute_min": "0.1.0", "features": [] },
            "capabilities": {
                "ports": { "read": ["missing-port"], "write": [] }
            }
        }))
        .unwrap(),
    )
    .expect("write manifest");
    fs::write(
        root.join("app.ipynb"),
        serde_json::to_string(&notebook).unwrap(),
    )
    .expect("write notebook");

    let result = notebook_app_doctor::call(&deps(), json!({ "path": root.to_string_lossy() }))
        .await
        .expect("doctor call succeeds");

    let body = structured(result);
    assert!(
        has_fail(&body),
        "expected ok=false for undeclared port: {body}"
    );
    assert!(
        has_check_level(&body, "port:missing-port", "fail"),
        "expected port:missing-port fail: {body}"
    );
}

// ── check 6: port store reachable ─────────────────────────────────────────────

#[tokio::test]
async fn doctor_check6_warn_when_port_store_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(root.join("spur-app.json"), minimal_manifest("app.ipynb")).expect("write manifest");
    fs::write(root.join("app.ipynb"), minimal_notebook()).expect("write notebook");
    // Do NOT create the port store — it is absent (app never ran)

    let result = notebook_app_doctor::call(&deps(), json!({ "path": root.to_string_lossy() }))
        .await
        .expect("doctor call succeeds");

    let body = structured(result);
    assert!(
        has_check_level(&body, "port_store", "warn"),
        "expected port_store:warn when store absent: {body}"
    );
}

// ── check 7: runtime.features deprecation ────────────────────────────────────

#[tokio::test]
async fn doctor_check7_warn_on_runtime_features() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::write(
        root.join("spur-app.json"),
        serde_json::to_string(&json!({
            "schema": "spur.app/v1",
            "name": "Old App",
            "entry_notebook": "app.ipynb",
            "open_mode": "app",
            "runtime": {
                "jute_min": "0.1.0",
                "features": ["some-feature"]
            }
        }))
        .unwrap(),
    )
    .expect("write manifest");
    fs::write(root.join("app.ipynb"), minimal_notebook()).expect("write notebook");

    let result = notebook_app_doctor::call(&deps(), json!({ "path": root.to_string_lossy() }))
        .await
        .expect("doctor call succeeds");

    let body = structured(result);
    assert!(
        has_check_level(&body, "runtime_features", "warn"),
        "expected runtime_features:warn: {body}"
    );
}

// ── tool registration ─────────────────────────────────────────────────────────

#[test]
fn notebook_app_doctor_tool_is_registered() {
    let names = tools::tools()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect::<Vec<_>>();
    assert!(
        names.iter().any(|n| n == "notebook_app_doctor"),
        "notebook_app_doctor must appear in tools list: {names:?}"
    );
}

// ── html_video integration path (happy path) ─────────────────────────────────

#[tokio::test]
async fn doctor_html_video_app_gallery_happy_path() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("app_gallery")
        .join("html_video");

    if !root.join("spur-app.json").is_file() {
        eprintln!("skipping html_video happy-path: app_gallery/html_video not present");
        return;
    }

    let result = notebook_app_doctor::call(&deps(), json!({ "path": root.to_string_lossy() }))
        .await
        .expect("doctor call succeeds for html_video");

    let body = structured(result);
    // Check 1 must pass (manifest is valid)
    assert!(
        has_check_level(&body, "manifest", "pass"),
        "html_video manifest must pass check 1: {body}"
    );
    // No capability check should be fail
    let fail_caps: Vec<_> = findings(body.clone())
        .into_iter()
        .filter(|f| {
            f["check"].as_str().unwrap_or("").starts_with("capability:") && f["level"] == "fail"
        })
        .collect();
    assert!(
        fail_caps.is_empty(),
        "html_video should have no failing capability checks: {fail_caps:?}"
    );
}

fn structured(result: CallToolResult) -> Value {
    assert_eq!(result.is_error, Some(false));
    result.structured_content.expect("structured content")
}
