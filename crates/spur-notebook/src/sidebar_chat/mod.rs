//! Sidebar AI Agent chat. Reuses the shipped ACP session + `AcpAgentBackend`
//! drain primitives; see
//! `docs/superpowers/specs/2026-06-09-notebook-sidebar-ai-agent-design.md`.

pub mod manager;
pub mod scope;
pub mod types;

#[cfg(test)]
mod drift_pin {
    // Compile-time assertion that the spec's reused APIs still exist at HEAD.
    #[allow(unused_imports)]
    use agent_client_protocol::schema::{McpServer, SessionNotification, SessionUpdate};
    #[allow(unused_imports)]
    use spur_acp::connection::AgentConnection;
    #[allow(unused_imports)]
    use spur_acp::types::PermissionRequest;

    #[allow(unused_imports)]
    use crate::dag::ai::acp_backend::AcpAgentBackend;

    #[test]
    fn apis_present() {
        // Trait methods used by the manager (signature pin):
        //   AgentConnection::new_session(&mut self, cwd: PathBuf, mcp_servers: Vec<McpServer>)
        //   AgentConnection::load_session(&mut self, req: LoadSessionRequest) -> Stream<SessionNotification>
        //   AgentConnection::list_sessions(&mut self) ...
        //   AgentConnection::prompt(&mut self, req) -> Stream<SessionNotification>
        //   AgentConnection::cancel(&mut self, session_id)
        // If any signature changed, the manager tasks (3,4) will not compile.
        assert!(true);
    }
}
