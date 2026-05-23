use agent_client_protocol::schema::McpServer;

#[test]
fn brain_mcp_servers_preinclude_notebook_stdio_proxy_on_stable_socket() {
    let servers = spur_core::notebook::brain_mcp_servers("http://127.0.0.1:3939/mcp");

    assert!(servers.iter().any(|server| match server {
        McpServer::Http(http) => http.name == "spur-mcp",
        _ => false,
    }));

    let notebook = servers
        .iter()
        .find_map(|server| match server {
            McpServer::Stdio(stdio) if stdio.name == "notebook" => Some(stdio),
            _ => None,
        })
        .expect("notebook MCP server should be preconfigured");

    assert_eq!(notebook.args[0], "--mcp-proxy");
    assert!(notebook.args[1].ends_with("/.spur/notebooks/control.sock"));
}
