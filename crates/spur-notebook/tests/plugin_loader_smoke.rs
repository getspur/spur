use std::{collections::BTreeMap, time::Duration};

use serde_json::{json, Value};
use spur_notebook::mcp::plugin_loader::{PluginConfig, PluginRegistry};
use tokio::{fs, process::Command, time::timeout};

#[tokio::test]
async fn plugin_loader_smoke() {
    if !command_succeeds("python3", &["--version"]).await {
        eprintln!("skipping plugin_loader_smoke: python3 is unavailable");
        return;
    }

    if !command_succeeds("python3", &["-c", "import mcp"]).await {
        eprintln!("skipping plugin_loader_smoke: Python mcp package is unavailable");
        return;
    }

    let temp = tempfile::Builder::new()
        .prefix("spur-notebook-plugin-loader-")
        .tempdir()
        .expect("temp dir");
    let server_path = temp.path().join("server.py");

    fs::write(&server_path, PYTHON_ECHO_SERVER)
        .await
        .expect("write inline Python MCP server");

    let config = PluginConfig {
        name: "hello-world".to_string(),
        server_type: "python".to_string(),
        entry: "server.py".to_string(),
        requirements: None,
        env: BTreeMap::new(),
        working_dir: temp.path().to_path_buf(),
    };

    let mut plugin_registry = PluginRegistry::new();
    let tool_names = plugin_registry
        .spawn(config)
        .await
        .expect("spawn hello-world plugin");
    assert!(tool_names.iter().any(|name| name == "echo"));
    assert!(plugin_registry.has_tool("echo"));

    let tools = plugin_registry
        .list_tools("hello-world")
        .await
        .expect("list hello-world tools");
    assert!(tools.iter().any(|tool| tool.name == "echo"));

    let response = plugin_registry
        .call_tool("hello-world", "echo", json!({ "message": "hi" }))
        .await
        .expect("call echo tool");
    assert!(
        json_contains_string(&response, "hi"),
        "expected echo response to contain input, got {response}"
    );

    timeout(
        Duration::from_secs(5),
        plugin_registry.shutdown("hello-world"),
    )
    .await
    .expect("plugin shutdown must not hang")
    .expect("plugin shutdown succeeds");

    assert!(!plugin_registry.has_tool("echo"));
}

const PYTHON_ECHO_SERVER: &str = r#"
from mcp.server.fastmcp import FastMCP

mcp = FastMCP("hello-world")

@mcp.tool()
def echo(message: str) -> str:
    """Return the input message unchanged."""
    return message

if __name__ == "__main__":
    mcp.run(transport="stdio")
"#;

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

fn json_contains_string(value: &Value, needle: &str) -> bool {
    if value.as_str().is_some_and(|text| text.contains(needle)) {
        return true;
    }

    if value
        .as_array()
        .is_some_and(|items| items.iter().any(|item| json_contains_string(item, needle)))
    {
        return true;
    }

    value.as_object().is_some_and(|object| {
        object
            .values()
            .any(|nested| json_contains_string(nested, needle))
    })
}
