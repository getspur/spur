use std::path::PathBuf;

use agent_client_protocol::schema::{McpServer, McpServerHttp, McpServerStdio};

pub fn control_socket_path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".spur").join("notebooks").join("control.sock")
}

pub fn notebook_binary_path() -> PathBuf {
    if let Some(path) = std::env::var_os("SPUR_NOTEBOOK_BIN") {
        return PathBuf::from(path);
    }

    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(dir) = current_exe.parent() {
            let sibling = dir.join("spur-notebook");
            if sibling.exists() {
                return sibling;
            }
        }
    }

    PathBuf::from("spur-notebook")
}

pub fn notebook_mcp_server() -> McpServer {
    McpServer::Stdio(
        McpServerStdio::new("notebook", notebook_binary_path()).args(vec![
            "--mcp-proxy".to_string(),
            control_socket_path().display().to_string(),
        ]),
    )
}

pub fn brain_mcp_servers(spur_mcp_url: &str) -> Vec<McpServer> {
    vec![
        McpServer::Http(McpServerHttp::new("spur-mcp", spur_mcp_url)),
        notebook_mcp_server(),
    ]
}
