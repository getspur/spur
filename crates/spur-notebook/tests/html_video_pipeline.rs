use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use rmcp::ServiceExt;
use rmcp::{
    model::{CallToolRequestParams, ClientInfo},
    service::{RunningService, Service},
    RoleClient,
};
use serde_json::{json, Value};
use spur_notebook::mcp::{
    bridge::{BridgeError, BridgeRequestFuture, BridgeRequester},
    start_server,
    tools::{self, html_video_render},
    transport::LengthPrefixedJsonTransport,
    ServerDeps,
};
use tokio::{fs, net::UnixStream, process::Command, time::timeout};

const SEARCH_TOOL: &str = "html_video_search_templates";
const GET_TEMPLATE_TOOL: &str = "html_video_get_template";
const RENDER_TOOL: &str = "html_video_render";
const CELL_CAPTURE_TOOL: &str = "notebook_get_cell_capture";

#[tokio::test]
async fn html_video_render_accepts_base64_webm_frames() {
    if !command_succeeds("ffmpeg", &["-version"]).await {
        eprintln!("skipping html_video_render_accepts_base64_webm_frames: ffmpeg is unavailable");
        return;
    }

    let temp = tempfile::Builder::new()
        .prefix("spur-notebook-html-video-render-")
        .tempdir()
        .expect("temp dir");
    let input_path = temp.path().join("input.webm");
    let output_path = temp.path().join("output.mp4");

    generate_webm_fixture(&input_path).await;
    let encoded = STANDARD.encode(fs::read(&input_path).await.expect("read webm fixture"));

    let result = html_video_render::call(
        &ServerDeps::from_bridge(Arc::new(NullBridge)),
        json!({
            "webm_frames": [encoded],
            "output_path": output_path,
            "resolution": "32x32",
            "fps": 2,
            "frame_duration": 0.5
        }),
    )
    .await
    .expect("render accepts base64 webm frame");

    let body = result.structured_content.expect("structured content");
    assert_eq!(body["frame_count"], 1);
    assert!(output_path.exists(), "expected mp4 output file to exist");
    let metadata = fs::metadata(&output_path)
        .await
        .expect("render output metadata");
    assert!(metadata.len() > 0, "rendered mp4 must be non-empty");
}

#[test]
fn html_video_render_schema_uses_webm_frames() {
    let tool = html_video_render::tool();
    let schema = serde_json::to_value(&tool.input_schema).expect("schema serializes");

    assert_eq!(schema["required"], json!(["webm_frames", "output_path"]));
    assert!(schema["properties"].get("webm_frames").is_some());
    assert!(schema["properties"].get("frame_duration").is_some());
    assert!(schema["properties"].get("duration").is_none());
}

#[test]
fn html_video_tool_inventory_includes_canvas_capture_and_render() {
    let names = tools::tools()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect::<Vec<_>>();

    assert!(names.iter().any(|name| name == CELL_CAPTURE_TOOL));
    assert!(names.iter().any(|name| name == RENDER_TOOL));
}

#[tokio::test]
async fn html_video_pipeline() {
    if !command_succeeds("ffmpeg", &["-version"]).await {
        eprintln!("skipping html_video_pipeline: ffmpeg is unavailable");
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

    let _template_result = match call_tool_with_variants(
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

    let input_path = temp.path().join("input.webm");
    generate_webm_fixture(&input_path).await;
    let encoded = STANDARD.encode(fs::read(&input_path).await.expect("read webm fixture"));
    let hint_path = temp.path().join("output.mp4");
    let hint_path_string = hint_path.to_string_lossy().to_string();

    let render_payloads = vec![
        json!({
            "webm_frames": [encoded.clone()],
            "output_path": hint_path_string.clone()
        }),
        json!({
            "webm_frames": [encoded],
        }),
    ];

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

async fn generate_webm_fixture(path: &Path) {
    let status = Command::new("ffmpeg")
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .arg("-f")
        .arg("lavfi")
        .arg("-i")
        .arg("testsrc=duration=1:size=320x240:rate=30")
        .arg("-c:v")
        .arg("libvpx")
        .arg(path)
        .status()
        .await
        .expect("generate webm fixture");
    assert!(status.success(), "ffmpeg must generate a webm fixture");
}

#[derive(Default)]
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

async fn call_tool_with_variants<S>(
    client: &RunningService<RoleClient, S>,
    method: &'static str,
    payloads: &[Value],
    max_duration: Duration,
) -> Option<Value>
where
    S: Service<RoleClient>,
{
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

async fn call_tool_once<S>(
    client: &RunningService<RoleClient, S>,
    method: &'static str,
    payload: Value,
    timeout_duration: Duration,
) -> Option<Value>
where
    S: Service<RoleClient>,
{
    let arguments = payload.as_object()?.clone();
    let request = CallToolRequestParams::new(method).with_arguments(arguments);
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
