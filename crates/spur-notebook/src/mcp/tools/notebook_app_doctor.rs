//! `notebook_app_doctor` — host-side conformance checker for Spur app bundles.
//!
//! Input:  `{ "path": "<app-root-dir-or-entry-notebook>" }`
//! Output: `{ "findings": [{check, level, message, location?}], "ok": bool }`
//!         where `ok` = no findings with level `"fail"`.
//!
//! Checks (spec §7, v1):
//!  1. `spur-app.json` parses; schema == "spur.app/v1"; `entry_notebook` exists.
//!  2. Every declared capability is known/grantable (serde deny_unknown_fields).
//!  3. Declared `capabilities.ports.read` names each exist as a DAG source in
//!     the entry notebook.
//!  4. Plugin spawn check (best-effort — skipped/warn if python3/uv absent).
//!  5. Skill check: skill file exists; HARD-GATE tool names present in live
//!     tool surface (downgraded to warn when check 4 was skipped).
//!  6. Port store reachable: warn if absent (app never ran), pass if ok.
//!  7. `runtime.features` non-empty → warn deprecated.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use rmcp::{
    model::{object as rmcp_object, CallToolResult, Tool},
    ErrorData as McpError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::process::Command;

use crate::{
    dag::notebook_port_root,
    mcp::ServerDeps,
    spur_app::{SpurAppManifest, SPUR_APP_MANIFEST, SPUR_APP_SCHEMA},
};

const METHOD: &str = "notebook_app_doctor";

/// A single conformance finding.
#[derive(Debug)]
struct Finding {
    check: String,
    level: &'static str, // "pass" | "warn" | "fail"
    message: String,
    location: Option<String>,
}

impl Finding {
    fn pass(check: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            check: check.into(),
            level: "pass",
            message: message.into(),
            location: None,
        }
    }

    fn warn(check: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            check: check.into(),
            level: "warn",
            message: message.into(),
            location: None,
        }
    }

    fn fail(check: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            check: check.into(),
            level: "fail",
            message: message.into(),
            location: None,
        }
    }

    fn with_location(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    fn to_value(&self) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("check".to_string(), json!(self.check));
        m.insert("level".to_string(), json!(self.level));
        m.insert("message".to_string(), json!(self.message));
        if let Some(loc) = &self.location {
            m.insert("location".to_string(), json!(loc));
        }
        Value::Object(m)
    }
}

pub fn tool() -> Tool {
    Tool::new(
        METHOD,
        "Run host-side conformance checks on a Spur app bundle.",
        rmcp_object(json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {
                    "type": "string",
                    "minLength": 1,
                    "description": "App root directory or path to the entry .ipynb notebook."
                }
            },
            "additionalProperties": false
        })),
    )
}

pub async fn call(_deps: &ServerDeps, arguments: Value) -> Result<CallToolResult, McpError> {
    #[derive(Deserialize)]
    struct Params {
        path: String,
    }

    let params: Params = serde_json::from_value(arguments).map_err(|e| {
        McpError::invalid_params(
            format!("{METHOD} requires {{ path }}"),
            Some(json!({ "error": e.to_string() })),
        )
    })?;

    let input_path = PathBuf::from(&params.path);
    let app_root = resolve_app_root(&input_path);

    let mut findings: Vec<Finding> = Vec::new();

    // ── Check 1: manifest + schema + entry_notebook ────────────────────────────

    let manifest_path = app_root.join(SPUR_APP_MANIFEST);
    let manifest_raw = match tokio::fs::read_to_string(&manifest_path).await {
        Ok(s) => s,
        Err(_) => {
            findings.push(
                Finding::fail(
                    "manifest",
                    format!("spur-app.json not found at {}", manifest_path.display()),
                )
                .with_location(manifest_path.display().to_string()),
            );
            return ok_result(findings);
        }
    };

    // First deserialize leniently to check schema/entry before deny_unknown
    // on capabilities.
    let manifest_value: Value = match serde_json::from_str(&manifest_raw) {
        Ok(v) => v,
        Err(e) => {
            findings.push(
                Finding::fail("manifest", format!("spur-app.json is not valid JSON: {e}"))
                    .with_location(manifest_path.display().to_string()),
            );
            return ok_result(findings);
        }
    };

    // Check schema field
    let schema = manifest_value["schema"].as_str().unwrap_or("");
    if schema != SPUR_APP_SCHEMA {
        findings.push(
            Finding::fail(
                "manifest",
                format!("schema is {:?}, expected {:?}", schema, SPUR_APP_SCHEMA),
            )
            .with_location(manifest_path.display().to_string()),
        );
        return ok_result(findings);
    }

    // Check entry_notebook exists
    let entry_notebook = manifest_value["entry_notebook"].as_str().unwrap_or("");
    if entry_notebook.is_empty() {
        findings.push(
            Finding::fail("manifest", "entry_notebook is missing or empty")
                .with_location(manifest_path.display().to_string()),
        );
        return ok_result(findings);
    }
    let entry_path = app_root.join(entry_notebook);
    if !entry_path.is_file() {
        findings.push(
            Finding::fail(
                "manifest",
                format!(
                    "entry_notebook {:?} does not exist at {}",
                    entry_notebook,
                    entry_path.display()
                ),
            )
            .with_location(entry_path.display().to_string()),
        );
        return ok_result(findings);
    }

    findings.push(Finding::pass(
        "manifest",
        "spur-app.json is valid, schema matches, entry_notebook exists",
    ));

    // ── Check 2: capabilities deserialization (deny_unknown_fields) ───────────

    // Try to deserialize the full manifest including capability validation.
    let cap_check_result: Result<SpurAppManifest, _> = serde_json::from_str(&manifest_raw);
    match cap_check_result {
        Err(e) => {
            let message = e.to_string();
            // serde's deny_unknown_fields error message contains the field name
            findings.push(
                Finding::fail(
                    "capability:unknown",
                    format!("unknown capability key: {message}"),
                )
                .with_location(manifest_path.display().to_string()),
            );
        }
        Ok(manifest) => {
            // Emit a pass finding for each declared capability
            let caps = &manifest.capabilities;
            if caps.active_output_scripts {
                findings.push(Finding::pass(
                    "capability:active_output_scripts",
                    "active_output_scripts is a known grantable capability",
                ));
            }
            if caps.canvas_capture {
                findings.push(Finding::pass(
                    "capability:canvas_capture",
                    "canvas_capture is a known grantable capability",
                ));
            }
            if caps.artifacts_dir {
                findings.push(Finding::pass(
                    "capability:artifacts_dir",
                    "artifacts_dir is a known grantable capability",
                ));
            }
            if caps.ports.is_some() {
                findings.push(Finding::pass(
                    "capability:ports",
                    "ports is a known grantable capability",
                ));
            }

            // ── Check 3: ports.read names exist as DAG sources ───────────────

            if let Some(ports) = &caps.ports {
                check_ports(&app_root, &entry_path, &ports.read, &mut findings).await;
            }

            // ── Check 6: port store reachable ────────────────────────────────

            check_port_store(&entry_path, &mut findings);

            // ── Check 4 + 5: plugin spawn + skill ───────────────────────────

            check_plugin_and_skill(&app_root, &manifest, _deps, &mut findings).await;

            // ── Check 7: runtime.features deprecation ────────────────────────

            if !manifest.runtime.features.is_empty() {
                findings.push(Finding::warn(
                    "runtime_features",
                    format!(
                        "runtime.features is deprecated (present: {:?}). Use capabilities instead.",
                        manifest.runtime.features
                    ),
                ));
            }
        }
    }

    ok_result(findings)
}

/// Resolve the app root from a path that may be either a directory or a
/// notebook file.
fn resolve_app_root(path: &Path) -> PathBuf {
    if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("ipynb") {
        path.parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    } else {
        path.to_path_buf()
    }
}

/// Check 3: each `ports.read` name exists as a DAG source in the entry
/// notebook.
async fn check_ports(
    _app_root: &Path,
    entry_path: &Path,
    read_ports: &[String],
    findings: &mut Vec<Finding>,
) {
    if read_ports.is_empty() {
        return;
    }

    let notebook_bytes = match tokio::fs::read(entry_path).await {
        Ok(b) => b,
        Err(e) => {
            findings.push(Finding::warn(
                "port:check",
                format!("could not read entry notebook for port check: {e}"),
            ));
            return;
        }
    };

    let notebook: Value = match serde_json::from_slice(&notebook_bytes) {
        Ok(v) => v,
        Err(e) => {
            findings.push(Finding::warn(
                "port:check",
                format!("entry notebook is not valid JSON: {e}"),
            ));
            return;
        }
    };

    // Collect all port names referenced in cell DAG metadata.
    let declared = collect_notebook_dag_ports(&notebook);

    for port in read_ports {
        if declared.contains(port.as_str()) {
            findings.push(Finding::pass(
                format!("port:{port}"),
                format!("port {port:?} is declared as a DAG source in the entry notebook"),
            ));
        } else {
            findings.push(
                Finding::fail(
                    format!("port:{port}"),
                    format!(
                        "port {port:?} is declared in capabilities.ports.read but has no \
                         matching DAG source (canvas-capture kind or produces[].port) in the \
                         entry notebook"
                    ),
                )
                .with_location(entry_path.display().to_string()),
            );
        }
    }
}

/// Extract all port names referenced in notebook cell DAG metadata.
///
/// A port is "declared" when either:
/// - a cell's `metadata.spur.dag.source.kind == "canvas-capture"` and
///   `metadata.spur.dag.source.port == <name>`, OR
/// - a cell's `metadata.spur.dag.produces[].port == <name>`.
fn collect_notebook_dag_ports(notebook: &Value) -> std::collections::HashSet<String> {
    let mut ports = std::collections::HashSet::new();

    let cells = notebook["cells"].as_array();
    let Some(cells) = cells else {
        return ports;
    };

    for cell in cells {
        let dag = &cell["metadata"]["spur"]["dag"];
        if dag.is_null() {
            continue;
        }

        // canvas-capture source
        let source = &dag["source"];
        if !source.is_null() {
            if let Some(port) = source["port"].as_str() {
                ports.insert(port.to_string());
            }
        }

        // produces[].port
        if let Some(produces) = dag["produces"].as_array() {
            for entry in produces {
                if let Some(port) = entry["port"].as_str() {
                    ports.insert(port.to_string());
                }
            }
        }
    }

    ports
}

/// Check 6: verify the port store directory is reachable.
fn check_port_store(entry_path: &Path, findings: &mut Vec<Finding>) {
    let ports_dir = notebook_port_root(entry_path).join("ports");
    if !ports_dir.exists() {
        findings.push(Finding::warn(
            "port_store",
            format!(
                "port store directory {} does not exist (app may never have run)",
                ports_dir.display()
            ),
        ));
        return;
    }

    let manifest_path = ports_dir.join("manifest.json");
    if manifest_path.is_file() {
        match std::fs::read_to_string(&manifest_path) {
            Ok(_) => {
                findings.push(Finding::pass(
                    "port_store",
                    format!("port store manifest found at {}", manifest_path.display()),
                ));
            }
            Err(e) => {
                findings.push(Finding::warn(
                    "port_store",
                    format!("port store manifest unreadable: {e}"),
                ));
            }
        }
    } else {
        findings.push(Finding::warn(
            "port_store",
            format!(
                "port store directory exists but manifest.json is absent ({})",
                manifest_path.display()
            ),
        ));
    }
}

/// Checks 4 + 5: spawn the app's plugin and verify tool names, then check the
/// skill file's HARD-GATE tool list.
async fn check_plugin_and_skill(
    app_root: &Path,
    manifest: &SpurAppManifest,
    _deps: &ServerDeps,
    findings: &mut Vec<Finding>,
) {
    // ── Check 4: plugin spawn ────────────────────────────────────────────────

    let Some(server) = manifest.mcp_server.as_ref() else {
        // No plugin declared — nothing to check.
        findings.push(Finding::pass(
            "plugin_spawn",
            "no mcp_server declared, plugin spawn check skipped",
        ));
        // Still check skill even without a plugin.
        check_skill(app_root, manifest, &[], true, findings).await;
        return;
    };

    // Best-effort: skip if interpreter unavailable.
    let interpreter_available = match server.server_type.as_str() {
        "python" => command_available("python3").await,
        "node" => command_available("node").await,
        other => {
            findings.push(Finding::warn(
                "plugin_spawn",
                format!("unsupported server_type {other:?}, spawn check skipped"),
            ));
            check_skill(app_root, manifest, &[], true, findings).await;
            return;
        }
    };

    if !interpreter_available {
        findings.push(Finding::warn(
            "plugin_spawn",
            format!(
                "interpreter for server_type {:?} unavailable, spawn check skipped",
                server.server_type
            ),
        ));
        // Downgrade skill check to warn if spawn was skipped.
        check_skill(app_root, manifest, &[], true, findings).await;
        return;
    }

    // Attempt to spawn and call tools/list.
    let config = crate::mcp::plugin_loader::PluginConfig::from_manifest(
        manifest.name.clone(),
        server,
        app_root,
    );

    let mut registry = crate::mcp::plugin_loader::PluginRegistry::new();
    match tokio::time::timeout(Duration::from_secs(60), registry.spawn(config)).await {
        Ok(Ok(tool_names)) => {
            findings.push(Finding::pass(
                "plugin_spawn",
                format!(
                    "plugin spawned and exposed {} tool(s): {:?}",
                    tool_names.len(),
                    tool_names
                ),
            ));
            registry.shutdown_all().await;
            check_skill(app_root, manifest, &tool_names, false, findings).await;
        }
        Ok(Err(e)) => {
            findings.push(Finding::warn(
                "plugin_spawn",
                format!("plugin spawn failed (best-effort): {e}"),
            ));
            check_skill(app_root, manifest, &[], true, findings).await;
        }
        Err(_) => {
            findings.push(Finding::warn(
                "plugin_spawn",
                "plugin spawn timed out after 60s (best-effort)",
            ));
            check_skill(app_root, manifest, &[], true, findings).await;
        }
    }
}

/// Check 5: skill file exists and HARD-GATE tool names are in the live surface.
async fn check_skill(
    app_root: &Path,
    manifest: &SpurAppManifest,
    plugin_tools: &[String],
    spawn_skipped: bool,
    findings: &mut Vec<Finding>,
) {
    let skill_rel = manifest.skill.as_deref().unwrap_or("skill/SKILL.md");
    let skill_path = app_root.join(skill_rel);

    if !skill_path.is_file() {
        findings.push(
            Finding::warn("skill", format!("skill file {:?} not found", skill_rel))
                .with_location(skill_path.display().to_string()),
        );
        return;
    }

    let skill_text = match tokio::fs::read_to_string(&skill_path).await {
        Ok(t) => t,
        Err(e) => {
            findings.push(Finding::warn(
                "skill",
                format!("could not read skill file: {e}"),
            ));
            return;
        }
    };

    findings.push(Finding::pass(
        "skill",
        format!("skill file {skill_rel:?} found"),
    ));

    // Extract backtick-quoted names from HARD-GATE block.
    let gate_tools = extract_hard_gate_tools(&skill_text);
    if gate_tools.is_empty() {
        findings.push(Finding::pass(
            "skill:hard_gate",
            "no HARD-GATE block or no backtick-quoted tool names found",
        ));
        return;
    }

    // Build the set of known tool names: notebook MCP tools + plugin tools.
    let notebook_tools: std::collections::HashSet<String> = crate::mcp::tools::tools()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();

    // Known notebook-tool-name prefixes that are gated in skills.
    let known_prefixes = ["notebook_", "notebook.", "html_video_"];

    for tool in &gate_tools {
        let starts_with_known_prefix = known_prefixes.iter().any(|p| tool.starts_with(p));
        if !starts_with_known_prefix {
            // Not a notebook/app tool name — skip (could be a placeholder text)
            continue;
        }

        let in_notebook = notebook_tools.contains(tool);
        let in_plugin = plugin_tools.iter().any(|n| n == tool);

        if in_notebook || in_plugin {
            findings.push(Finding::pass(
                format!("skill:tool:{tool}"),
                format!("HARD-GATE tool {tool:?} is present in the live tool surface"),
            ));
        } else if spawn_skipped {
            findings.push(Finding::warn(
                format!("skill:tool:{tool}"),
                format!(
                    "HARD-GATE tool {tool:?} not found in notebook tool registry \
                     (plugin spawn was skipped, cannot check plugin tools)"
                ),
            ));
        } else {
            findings.push(
                Finding::fail(
                    format!("skill:tool:{tool}"),
                    format!(
                        "HARD-GATE tool {tool:?} is referenced in skill but absent from \
                         both the notebook MCP tool registry and the plugin's tools/list"
                    ),
                )
                .with_location(skill_path.display().to_string()),
            );
        }
    }
}

/// Extract backtick-quoted tool names from the `<HARD-GATE>…</HARD-GATE>`
/// section of a skill file.
fn extract_hard_gate_tools(text: &str) -> Vec<String> {
    let start = match text.find("<HARD-GATE>") {
        Some(i) => i + "<HARD-GATE>".len(),
        None => return vec![],
    };
    let end = match text[start..].find("</HARD-GATE>") {
        Some(i) => start + i,
        None => return vec![],
    };

    let block = &text[start..end];
    let mut tools = Vec::new();
    let mut rest = block;
    while let Some(i) = rest.find('`') {
        rest = &rest[i + 1..];
        if let Some(j) = rest.find('`') {
            let name = rest[..j].trim().to_string();
            if !name.is_empty() && !name.contains('\n') {
                tools.push(name);
            }
            rest = &rest[j + 1..];
        }
    }

    tools
}

/// Returns `true` if the given command is available on PATH.
async fn command_available(cmd: &str) -> bool {
    tokio::time::timeout(
        Duration::from_secs(5),
        Command::new(cmd)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
    )
    .await
    .map(|r| r.map(|s| s.success()).unwrap_or(false))
    .unwrap_or(false)
}

/// Build the final `CallToolResult` from accumulated findings.
fn ok_result(findings: Vec<Finding>) -> Result<CallToolResult, McpError> {
    let ok = !findings.iter().any(|f| f.level == "fail");
    let findings_json: Vec<Value> = findings.iter().map(Finding::to_value).collect();
    Ok(CallToolResult::structured(json!({
        "ok": ok,
        "findings": findings_json
    })))
}
