//! M0 protocol-compat spike.
//!
//! Drives `claude-agent-acp` via `NativeAcpConnection` and reports which ACP
//! methods round-trip. Not a production artifact — delete after M0 gate passes.
//!
//! Run: cargo run -p spur-acp --example compat_spike -- <path-to-cwd>

use std::path::PathBuf;
use std::time::Instant;

// TODO: re-enable after Task 3 adds set_session_mode + authenticate to the trait
// use agent_client_protocol::{AuthenticateRequest, AuthMethodId, SetSessionModeRequest};
use agent_client_protocol::{
    ContentBlock, InitializeRequest, PromptRequest, ProtocolVersion, TextContent,
};
use spur_acp::connection::{AgentConnection, NativeAcpConnection};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("spur_acp=debug,compat_spike=info")
        .init();

    let cwd: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));

    println!("=== M0 compat spike: claude-agent-acp ===\n");

    let mut conn = NativeAcpConnection::new(
        "claude-code-acp-spike",
        "npx",
        vec![
            "--yes".to_string(),
            "@agentclientprotocol/claude-agent-acp@latest".to_string(),
        ],
        None,
    );

    let t0 = Instant::now();
    let init = conn
        .initialize(InitializeRequest::new(ProtocolVersion::LATEST))
        .await?;
    println!(
        "[ok] initialize in {:?}, protocol={:?}, caps={:?}",
        t0.elapsed(),
        init.protocol_version,
        init.agent_capabilities
    );

    let t1 = Instant::now();
    let session = conn.new_session(cwd.clone(), vec![]).await?;
    println!(
        "[ok] new_session in {:?}: {}",
        t1.elapsed(),
        session.session_id
    );

    let prompt_req = PromptRequest::new(
        session.session_id.clone(),
        vec![ContentBlock::Text(TextContent::new(
            "Say only the word OK and nothing else.".to_string(),
        ))],
    );
    let mut stream = conn.prompt(prompt_req).await?;
    use futures::StreamExt;
    let mut chunks = 0usize;
    while let Some(_notif) = stream.next().await {
        chunks += 1;
        if chunks > 200 {
            break;
        }
    }
    println!("[ok] prompt streamed {chunks} notifications");

    // TODO: re-enable after Task 3 adds set_session_mode + authenticate to the trait
    // let mode_req = SetSessionModeRequest::new(session.session_id.clone(), "plan");
    // match conn.set_session_mode(mode_req).await {
    //     Ok(_) => println!("[ok] set_session_mode(plan)"),
    //     Err(e) => println!("[WARN] set_session_mode failed: {e}"),
    // }

    // TODO: re-enable after Task 3 adds set_session_mode + authenticate to the trait
    // match conn
    //     .authenticate(AuthenticateRequest::new(AuthMethodId("claude-ai-login".into())))
    //     .await
    // {
    //     Ok(_) => println!("[ok] authenticate echoed"),
    //     Err(e) => println!("[info] authenticate returned: {e} (expected if not wired)"),
    // }

    conn.shutdown().await?;
    println!("\n=== spike complete ===");
    Ok(())
}
