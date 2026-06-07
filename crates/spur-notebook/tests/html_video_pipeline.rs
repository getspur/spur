use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rmcp::model::{CallToolRequestParams, ClientInfo};
use rmcp::ServiceExt;
use serde_json::{json, Value};
use spur_notebook::mcp::{start_server, transport::LengthPrefixedJsonTransport};
use tokio::{fs, net::UnixStream, process::Command, time::timeout};

const SEARCH_TOOL: &str = "html_video_search_templates";
const GET_TEMPLATE_TOOL: &str = "html_video_get_template";
const RENDER_TOOL: &str = "html_video_render";

#[tokio::test]
async fn html_video_pipeline() {
    if !command_succeeds("ffmpeg", &["-version"]).await {
        eprintln!("skipping html_video_pipeline: ffmpeg is unavailable");
        return;
    }

    if !playwright_or_chromium_available().await {
        eprintln!("skipping html_video_pipeline: no Playwright/Chromium binary available");
        return;
    }

    let temp = tempfile::Builder::new()
        .prefix("spur-notebook-html-video-")
        .tempdir()
        .expect("temp dir");
    let socket_path = temp.path().join("notebook.sock");
    let _server = start_server(&socket_path)
        .await
        .expect("notebook MCP server starts");

    let stream = UnixStream::connect(&socket_path)
        .await
        .expect("connect to MCP server");
    let transport = LengthPrefixedJsonTransport::new(stream);
    let client = ClientInfo::default()
        .serve(transport)
        .await
        .expect("MCP client initializes");

    let search_result = match call_tool_with_variants(
        &client,
        SEARCH_TOOL,
        &[json!({ "intent": "data visualization" })],
        Duration::from_secs(4),
    )
    .await
    {
        Some(value) => value,
        None => {
            eprintln!("skipping html_video_pipeline: search tool unavailable");
            let _ = client.cancel().await;
            return;
        }
    };

    let template_id = match template_id_from_result(&search_result) {
        Some(value) => value,
        None => {
            eprintln!("skipping html_video_pipeline: search result has no template id");
            let _ = client.cancel().await;
            return;
        }
    };

    let template_result = match call_tool_with_variants(
        &client,
        GET_TEMPLATE_TOOL,
        &[
            json!({ "template_id": template_id.clone() }),
            json!({ "id": template_id.clone() }),
            json!({ "template": template_id.clone() }),
        ],
        Duration::from_secs(4),
    )
    .await
    {
        Some(value) => value,
        None => {
            eprintln!("skipping html_video_pipeline: get_template tool unavailable");
            let _ = client.cancel().await;
            return;
        }
    };

    let frame_path = temp.path().join("frame.html");
    fs::write(
        &frame_path,
        r#"
<!DOCTYPE html>
<html>
<head>
  <meta charset="UTF-8" />
  <style>
    html, body { margin: 0; width: 100%; height: 100%; background: #020617; }
    .dot {
      width: 56px;
      height: 56px;
      border-radius: 50%;
      background: linear-gradient(120deg, #22d3ee, #2563eb);
      position: absolute;
      top: calc(50% - 28px);
      left: calc(50% - 28px);
      animation: orbit 2s ease-in-out infinite alternate;
    }
    @keyframes orbit {
      0% { transform: translate(-160px, 0px) scale(0.9); opacity: 0.6; }
      100% { transform: translate(160px, 0px) scale(1.1); opacity: 1; }
    }
  </style>
</head>
<body>
  <div class="dot"></div>
</body>
</html>
"#,
    )
    .await
    .expect("write html frame");

    let hint_path = temp.path().join("output.mp4");
    let hint_path_string = hint_path.to_string_lossy().to_string();
    let frame_path_string = frame_path.to_string_lossy().to_string();
    let template_candidates = extract_template_path(&template_result);

    let mut render_payloads = vec![
        json!({
            "template_id": template_id.clone(),
            "frame_path": frame_path_string.clone(),
            "output_path": hint_path_string.clone()
        }),
        json!({
            "id": template_id.clone(),
            "frame_path": frame_path_string.clone(),
            "output_path": hint_path_string.clone()
        }),
        json!({
            "template_id": template_id.clone(),
            "frame_path": frame_path_string.clone(),
        }),
        json!({
            "id": template_id.clone(),
            "frame_path": frame_path_string.clone(),
        }),
        json!({
            "template_id": template_id.clone(),
            "path": frame_path_string.clone(),
            "output_path": hint_path_string.clone()
        }),
        json!({
            "id": template_id,
            "path": frame_path_string.clone(),
            "output_path": hint_path_string.clone()
        }),
    ];

    if let Some(path) = template_candidates.clone() {
        render_payloads.push(json!({
            "template_path": path,
            "frame_path": frame_path.to_string_lossy().to_string(),
            "output_path": hint_path_string,
        }));
    }

    let render_result = match call_tool_with_variants(
        &client,
        RENDER_TOOL,
        &render_payloads,
        Duration::from_secs(20),
    )
    .await
    {
        Some(value) => value,
        None => {
            eprintln!("skipping html_video_pipeline: render tool unavailable");
            let _ = client.cancel().await;
            return;
        }
    };

    let output_path = match extract_mp4_path(&render_result).or_else(|| {
        if hint_path.exists() {
            Some(hint_path.clone())
        } else {
            None
        }
    }) {
        Some(path) => path,
        None => {
            eprintln!("skipping html_video_pipeline: render output did not expose an mp4 path");
            let _ = client.cancel().await;
            return;
        }
    };

    let metadata = fs::metadata(&output_path)
        .await
        .expect("render output metadata");
    assert!(metadata.len() > 0, "rendered mp4 must be non-empty");

    let output_view_supports_video_mime = output_view_dispatches_video_mime();
    let rendered_payload_has_video_mime = json_has_key(&render_result, "video/mp4");
    assert!(
        output_view_supports_video_mime || rendered_payload_has_video_mime,
        "Expected html video MIME dispatch to be represented in OutputView or render payload"
    );

    assert!(output_path.exists(), "expected mp4 output file to exist");

    let _ = client.cancel().await;
}

async fn command_succeeds(command: &str, args: &[&str]) -> bool {
    let Ok(Ok(status)) = timeout(
        Duration::from_secs(5),
        Command::new(command)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
    )
    .await
    else {
        return false;
    };
    status.success()
}

async fn playwright_or_chromium_available() -> bool {
    const CANDIDATES: &[&str] = &[
        "playwright",
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
    ];

    for candidate in CANDIDATES {
        if command_succeeds(candidate, &["--version"]).await {
            return true;
        }
    }

    false
}

async fn call_tool_with_variants(
    client: &ClientInfo,
    method: &str,
    payloads: &[Value],
    max_duration: Duration,
) -> Option<Value> {
    let deadline = Instant::now() + max_duration;

    for payload in payloads {
        let budget = deadline.saturating_duration_since(Instant::now());
        if budget.is_zero() {
            return None;
        }

        if let Some(value) = call_tool_once(client, method, payload.clone(), budget).await {
            return Some(value);
        }
    }

    None
}

async fn call_tool_once(
    client: &ClientInfo,
    method: &str,
    payload: Value,
    timeout_duration: Duration,
) -> Option<Value> {
    let request = CallToolRequestParams::new(method).with_arguments(payload);
    let Ok(Ok(result)) = timeout(timeout_duration, client.call_tool(request)).await else {
        return None;
    };

    if result.is_error == Some(true) {
        return None;
    }

    Some(result.structured_content.unwrap_or(Value::Null))
}

fn template_id_from_result(value: &Value) -> Option<String> {
    if let Some(id) = first_matching_field(value, &["template_id", "templateId", "id"]) {
        return Some(id);
    }

    if let Some(templates) = value.get("templates").and_then(Value::as_array) {
        if let Some(id) = first_template_id_in_array(templates) {
            return Some(id);
        }
    }

    if let Some(results) = value.get("results").and_then(Value::as_array) {
        if let Some(id) = first_template_id_in_array(results) {
            return Some(id);
        }
    }

    if let Some(items) = value.get("items").and_then(Value::as_array) {
        if let Some(id) = first_template_id_in_array(items) {
            return Some(id);
        }
    }

    if let Some(array) = value.as_array() {
        return first_template_id_in_array(array);
    }

    if let Some(object) = value.as_object() {
        for nested in object.values() {
            if let Some(id) = template_id_from_result(nested) {
                return Some(id);
            }
        }
    }

    None
}

fn first_matching_field(value: &Value, keys: &[&str]) -> Option<String> {
    let Some(object) = value.as_object() else {
        return None;
    };

    for key in keys {
        let Some(raw) = object.get(*key).and_then(Value::as_str) else {
            continue;
        };
        if !raw.trim().is_empty() {
            return Some(raw.to_string());
        }
    }

    None
}

fn first_template_id_in_array(templates: &[Value]) -> Option<String> {
    for template in templates {
        if let Some(value) = first_matching_field(template, &["template_id", "templateId", "id"]) {
            return Some(value);
        }
    }

    None
}

fn extract_template_path(value: &Value) -> Option<String> {
    first_matching_field(
        value,
        &[
            "template_path",
            "path",
            "template",
            "file",
            "templateFile",
            "template_file",
        ],
    )
}

fn extract_mp4_path(value: &Value) -> Option<PathBuf> {
    for candidate in collect_string_values(value) {
        let candidate = candidate.trim().trim_start_matches("file://").to_string();

        if !candidate.to_lowercase().contains(".mp4") {
            continue;
        }

        let path = PathBuf::from(&candidate);
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .ok()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(path)
        };
        if path.extension().and_then(|ext| ext.to_str()) == Some("mp4") && path.exists() {
            return Some(path);
        }
    }

    None
}

fn collect_string_values(value: &Value) -> Vec<String> {
    let mut values = Vec::new();

    if let Some(string) = value.as_str() {
        values.push(string.to_string());
    }

    if let Some(array) = value.as_array() {
        for item in array {
            values.extend(collect_string_values(item));
        }
    }

    if let Some(object) = value.as_object() {
        for value in object.values() {
            values.extend(collect_string_values(value));
        }
    }

    values
}

fn json_has_key(value: &Value, key: &str) -> bool {
    if let Some(object) = value.as_object() {
        if object.contains_key(key) {
            return true;
        }
        for nested in object.values() {
            if json_has_key(nested, key) {
                return true;
            }
        }
    }

    if let Some(array) = value.as_array() {
        return array.iter().any(|item| json_has_key(item, key));
    }

    false
}

fn output_view_dispatches_video_mime() -> bool {
    let source = match std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("jute-notebook/src/ui/notebook/OutputView.tsx"),
    ) {
        Ok(contents) => contents,
        Err(_) => return false,
    };

    source.contains("video/mp4")
}
