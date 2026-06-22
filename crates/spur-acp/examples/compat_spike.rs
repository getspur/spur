//! M0 protocol-compat spike.
//!
//! Drives `claude-agent-acp` via `NativeAcpConnection` and reports which ACP
//! methods round-trip. Not a production artifact — delete after M0 gate passes.
//!
//! Run: cargo run -p spur-acp --example compat_spike -- <path-to-cwd>

use std::path::PathBuf;
use std::time::Instant;

use agent_client_protocol::schema::{
    AuthMethodId, AuthenticateRequest, ContentBlock, InitializeRequest, ListSessionsRequest,
    LoadSessionRequest, PromptRequest, ProtocolVersion, SetSessionModeRequest, TextContent,
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

    // Subscribe BEFORE new_session so notifications emitted during session
    // setup (e.g. claude-code-acp's initial `available_commands_update`)
    // land on a live receiver. tokio::sync::broadcast::send returns Err
    // if there are zero current receivers — no retention for late
    // subscribers — so ordering matters.
    let notif_rx = conn.subscribe_session_notifications();

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

    let _stream = conn.prompt(prompt_req).await?;

    let mut chunks = 0usize;
    if let Some(mut rx) = notif_rx {
        // Prompt has returned; drain buffered broadcast items with a
        // 500ms cap for any LocalSet-scheduled stragglers.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(_notif)) => {
                    chunks += 1;
                    if chunks > 200 {
                        break;
                    }
                }
                Ok(Err(_)) | Err(_) => break,
            }
        }
    }
    println!("[ok] prompt streamed {chunks} notifications");

    let mode_req = SetSessionModeRequest::new(session.session_id.clone(), "plan");
    match conn.set_session_mode(mode_req).await {
        Ok(_) => println!("[ok] set_session_mode(plan)"),
        Err(e) => println!("[WARN] set_session_mode failed: {e}"),
    }

    match conn
        .authenticate(AuthenticateRequest::new(AuthMethodId::new(
            "claude-ai-login",
        )))
        .await
    {
        Ok(_) => println!("[ok] authenticate echoed"),
        Err(e) => println!("[info] authenticate returned: {e} (expected if not wired)"),
    }

    // ── Phase 1: list_sessions + load_session evidence ──────────────────────
    // We want to know whether ACP list/load work against claude-agent-acp, so
    // we can decide whether Spur needs a filesystem fallback for Claude's
    // JSONL format.

    println!();
    println!("=== Phase 1: list_sessions + load_session ===");

    let t_list_none = Instant::now();
    let list_all = conn.list_sessions(ListSessionsRequest::new()).await?;
    println!(
        "[ok] list_sessions (no cwd) in {:?}: {} sessions",
        t_list_none.elapsed(),
        list_all.sessions.len()
    );

    let t_list_cwd = Instant::now();
    let list_scoped = conn
        .list_sessions(ListSessionsRequest::new().cwd(cwd.clone()))
        .await?;
    println!(
        "[ok] list_sessions (cwd={}) in {:?}: {} sessions",
        cwd.display(),
        t_list_cwd.elapsed(),
        list_scoped.sessions.len()
    );

    // Pick a historical session (skip the one we just created in this run).
    let target = list_scoped
        .sessions
        .iter()
        .find(|s| s.session_id.0 != session.session_id.0);
    if let Some(first) = target {
        println!(
            "     first: id={} cwd={} title={:?}",
            first.session_id.0,
            first.cwd.display(),
            first.title
        );

        let t_load = Instant::now();
        let load_req = LoadSessionRequest::new(first.session_id.clone(), first.cwd.clone());
        // Fresh subscriber for the load-history replay. Subscribe BEFORE
        // the load call so early replay items aren't missed.
        let load_notif_rx = conn.subscribe_session_notifications();
        match conn.load_session(load_req).await {
            Ok((_load_response, _load_stream)) => {
                let mut replayed = 0usize;
                let mut variant_counts: std::collections::HashMap<&'static str, usize> =
                    std::collections::HashMap::new();
                if let Some(mut rx) = load_notif_rx {
                    // After load_session returns, drain buffered replay items
                    // with a 1s cap. Transports without a broadcast (stdio
                    // etc.) use the returned stream instead; this path is
                    // the native/broadcast code path.
                    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
                    loop {
                        let remaining =
                            deadline.saturating_duration_since(tokio::time::Instant::now());
                        if remaining.is_zero() {
                            break;
                        }
                        let notif = match tokio::time::timeout(remaining, rx.recv()).await {
                            Ok(Ok(n)) => n,
                            Ok(Err(_)) | Err(_) => break,
                        };
                        replayed += 1;
                        let name = match &notif.update {
                            agent_client_protocol::schema::SessionUpdate::UserMessageChunk(_) => {
                                "user_message_chunk"
                            }
                            agent_client_protocol::schema::SessionUpdate::AgentMessageChunk(_) => {
                                "agent_message_chunk"
                            }
                            agent_client_protocol::schema::SessionUpdate::AgentThoughtChunk(_) => {
                                "agent_thought_chunk"
                            }
                            agent_client_protocol::schema::SessionUpdate::ToolCall(_) => "tool_call",
                            agent_client_protocol::schema::SessionUpdate::ToolCallUpdate(_) => {
                                "tool_call_update"
                            }
                            agent_client_protocol::schema::SessionUpdate::Plan(_) => "plan",
                            agent_client_protocol::schema::SessionUpdate::AvailableCommandsUpdate(_) => {
                                "available_commands_update"
                            }
                            agent_client_protocol::schema::SessionUpdate::CurrentModeUpdate(_) => {
                                "current_mode_update"
                            }
                            _ => "other",
                        };
                        *variant_counts.entry(name).or_insert(0) += 1;
                        if replayed >= 5000 {
                            break;
                        }
                    }
                }
                println!(
                    "[ok] load_session streamed {replayed} notifications in {:?}",
                    t_load.elapsed()
                );
                let mut kinds: Vec<(&str, usize)> = variant_counts.into_iter().collect();
                kinds.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
                for (k, v) in kinds {
                    println!("     {k}: {v}");
                }
            }
            Err(e) => println!("[WARN] load_session failed: {e}"),
        }
    } else {
        println!(
            "[info] no sessions found for cwd={} — skipping load_session test",
            cwd.display()
        );
    }

    conn.shutdown().await?;
    println!("\n=== spike complete ===");
    Ok(())
}
