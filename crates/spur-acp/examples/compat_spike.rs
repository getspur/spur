//! ACP capability/usage compatibility probe.
//!
//! Drives an ACP agent via `NativeAcpConnection` and reports which ACP methods
//! and response fields round-trip. Defaults to `claude-agent-acp` through `npx`.
//!
//! Run: scripts/spur-cargo run -p spur-acp --example compat_spike -- <path-to-cwd>
//! Run custom agent:
//! scripts/spur-cargo run -p spur-acp --example compat_spike -- --cwd <path> -- <agent-cmd> <args...>

use std::path::PathBuf;
use std::time::Instant;

use agent_client_protocol::schema::v1::{
    AuthMethodId, AuthenticateRequest, CloseSessionRequest, ContentBlock, DeleteSessionRequest,
    InitializeRequest, InitializeResponse, ListSessionsRequest, LoadSessionRequest,
    NewSessionResponse, PromptRequest, ResumeSessionRequest, SetSessionModeRequest, TextContent,
};
use agent_client_protocol::schema::ProtocolVersion;
use spur_acp::connection::{AgentConnection, NativeAcpConnection};
use spur_acp::{extract_choices, AcpError, AgentKind, SpurAgentCaps, Usage};

const DEFAULT_AGENT_NAME: &str = "claude-code-acp-spike";
const DEFAULT_AGENT_COMMAND: &str = "npx";
const DEFAULT_AGENT_ARGS: &[&str] = &["--yes", "@agentclientprotocol/claude-agent-acp@latest"];

#[derive(Debug)]
struct ProbeArgs {
    cwd: PathBuf,
    agent_name: String,
    agent_kind: AgentKind,
    command: String,
    args: Vec<String>,
}

impl ProbeArgs {
    fn parse() -> anyhow::Result<Self> {
        Self::parse_from(std::env::args().skip(1))
    }

    fn parse_from(args: impl IntoIterator<Item = String>) -> anyhow::Result<Self> {
        let mut cwd = None;
        let mut agent_name = None;
        let mut agent_kind = None;
        let mut command = None;
        let mut command_args = Vec::new();
        let mut it = args.into_iter();

        while let Some(arg) = it.next() {
            match arg.as_str() {
                "-h" | "--help" => anyhow::bail!("{USAGE}"),
                "--cwd" => cwd = Some(PathBuf::from(next_arg(&mut it, "--cwd")?)),
                "--agent" | "--agent-name" => agent_name = Some(next_arg(&mut it, &arg)?),
                "--agent-kind" => {
                    agent_kind = Some(AgentKind::from_name(&next_arg(&mut it, "--agent-kind")?));
                }
                "--cmd" | "--command" => command = Some(next_arg(&mut it, &arg)?),
                "--arg" => command_args.push(next_arg(&mut it, "--arg")?),
                "--" => {
                    command = Some(next_arg(&mut it, "--")?);
                    command_args.extend(it);
                    break;
                }
                _ if arg.starts_with("--") => anyhow::bail!("unknown argument {arg:?}\n{USAGE}"),
                _ if cwd.is_none() => cwd = Some(PathBuf::from(arg)),
                _ => anyhow::bail!(
                    "unexpected positional argument {arg:?}; use -- <agent-cmd> <args...>\n{USAGE}"
                ),
            }
        }

        let cwd = cwd.unwrap_or_else(|| std::env::current_dir().expect("cwd"));
        let using_default_agent = command.is_none();
        let command = command.unwrap_or_else(|| DEFAULT_AGENT_COMMAND.to_string());
        if using_default_agent {
            command_args = DEFAULT_AGENT_ARGS.iter().map(ToString::to_string).collect();
        }
        let agent_name = agent_name.unwrap_or_else(|| {
            if using_default_agent {
                DEFAULT_AGENT_NAME.to_string()
            } else {
                command.clone()
            }
        });
        let agent_kind = agent_kind.unwrap_or_else(|| {
            if using_default_agent {
                AgentKind::ClaudeCodeAcp
            } else {
                AgentKind::from_name(&agent_name)
            }
        });

        Ok(Self {
            cwd,
            agent_name,
            agent_kind,
            command,
            args: command_args,
        })
    }
}

const USAGE: &str = "\
Usage:
  compat_spike [<cwd>]
  compat_spike --cwd <path> [--agent-name <name>] [--agent-kind <kind>] -- <agent-cmd> <args...>
  compat_spike --cwd <path> --cmd <agent-cmd> [--arg <arg> ...]

Defaults to: npx --yes @agentclientprotocol/claude-agent-acp@latest";

fn next_arg(it: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<String> {
    it.next()
        .ok_or_else(|| anyhow::anyhow!("{flag} requires a value"))
}

#[cfg(test)]
fn format_probe_report(
    init: &InitializeResponse,
    session: &NewSessionResponse,
    usage: Option<&Usage>,
    agent_kind: AgentKind,
) -> String {
    [
        format_initialize_report(init),
        format_session_config_report(init, session, agent_kind),
        format_usage_report(usage),
    ]
    .join("\n")
}

fn format_initialize_report(init: &InitializeResponse) -> String {
    let caps = &init.agent_capabilities;
    let session = &caps.session_capabilities;
    format!(
        "\
=== initialize capability report ===
protocol_version: {:?}
load_session: {}
sessionCapabilities: list={} delete={} resume={} close={}",
        init.protocol_version,
        yes_no(caps.load_session),
        yes_no(session.list.is_some()),
        yes_no(session.delete.is_some()),
        yes_no(session.resume.is_some()),
        yes_no(session.close.is_some())
    )
}

fn format_session_config_report(
    init: &InitializeResponse,
    session: &NewSessionResponse,
    agent_kind: AgentKind,
) -> String {
    let caps = SpurAgentCaps::new(init, session, agent_kind);
    let options = session.config_options.as_deref().unwrap_or_default();
    let ids = options
        .iter()
        .map(|option| option.id.0.as_ref())
        .collect::<Vec<_>>()
        .join(", ");
    let ids = if ids.is_empty() {
        "none".to_string()
    } else {
        ids
    };
    let model = match caps.model_option() {
        Some(option) => {
            let choices = extract_choices(option).len();
            let category = option
                .category
                .as_ref()
                .map(|category| format!("{category:?}"))
                .unwrap_or_else(|| "none".to_string());
            if choices > 0 {
                format!(
                    "model option: yes id={} category={} choices={}",
                    option.id.0.as_ref(),
                    category,
                    choices
                )
            } else {
                format!(
                    "model option: no selectable choices id={} category={} choices=0",
                    option.id.0.as_ref(),
                    category
                )
            }
        }
        None => "model option: no model config option".to_string(),
    };

    format!(
        "\
=== new_session config report ===
config_options: {} [{}]
{}",
        options.len(),
        ids,
        model
    )
}

fn format_usage_report(usage: Option<&Usage>) -> String {
    match usage {
        Some(usage) => format!(
            "\
=== prompt usage report ===
usage: emitted total_tokens={} input_tokens={} output_tokens={} thought_tokens={:?} cached_read_tokens={:?} cached_write_tokens={:?}",
            usage.total_tokens,
            usage.input_tokens,
            usage.output_tokens,
            usage.thought_tokens,
            usage.cached_read_tokens,
            usage.cached_write_tokens
        ),
        None => "\
=== prompt usage report ===
usage: agent emitted no usage"
            .to_string(),
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn command_line(command: &str, args: &[String]) -> String {
    if args.is_empty() {
        command.to_string()
    } else {
        format!("{command} {}", args.join(" "))
    }
}

fn print_lifecycle_probe(method: &str, result: Result<(), AcpError>) {
    match result {
        Ok(()) => println!("[ok] {method} round-tripped"),
        Err(AcpError::CapabilityMissing(_)) => {
            println!("[info] {method}: not advertised (gated)");
        }
        Err(AcpError::Transport(e)) => println!("[WARN] {method} failed: {e}"),
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("spur_acp=debug,compat_spike=info")
        .init();

    let cli = ProbeArgs::parse()?;
    let cwd = cli.cwd.clone();

    println!("=== ACP capability/usage probe: {} ===", cli.agent_name);
    println!("cwd: {}", cwd.display());
    println!("agent command: {}\n", command_line(&cli.command, &cli.args));

    let mut conn = NativeAcpConnection::new_with_kind(
        &cli.agent_name,
        &cli.command,
        cli.args.clone(),
        cli.agent_kind,
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
    println!("{}", format_initialize_report(&init));

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
    println!(
        "{}",
        format_session_config_report(&init, &session, cli.agent_kind)
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
    let usage = conn.take_last_prompt_usage();
    println!("{}", format_usage_report(usage.as_ref()));

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

    let caps = SpurAgentCaps::new(&init, &session, cli.agent_kind);
    if caps.supports_list_sessions() {
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
        if caps.supports_load_session() {
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
                            let deadline =
                                tokio::time::Instant::now() + std::time::Duration::from_secs(1);
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
                                    agent_client_protocol::schema::v1::SessionUpdate::UserMessageChunk(_) => {
                                        "user_message_chunk"
                                    }
                                    agent_client_protocol::schema::v1::SessionUpdate::AgentMessageChunk(_) => {
                                        "agent_message_chunk"
                                    }
                                    agent_client_protocol::schema::v1::SessionUpdate::AgentThoughtChunk(_) => {
                                        "agent_thought_chunk"
                                    }
                                    agent_client_protocol::schema::v1::SessionUpdate::ToolCall(_) => "tool_call",
                                    agent_client_protocol::schema::v1::SessionUpdate::ToolCallUpdate(_) => {
                                        "tool_call_update"
                                    }
                                    agent_client_protocol::schema::v1::SessionUpdate::Plan(_) => "plan",
                                    agent_client_protocol::schema::v1::SessionUpdate::AvailableCommandsUpdate(_) => {
                                        "available_commands_update"
                                    }
                                    agent_client_protocol::schema::v1::SessionUpdate::CurrentModeUpdate(_) => {
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
        } else {
            println!("[info] load_session: not advertised (gated)");
        }
    } else {
        println!("[info] list_sessions: not advertised (gated)");
        if !caps.supports_load_session() {
            println!("[info] load_session: not advertised (gated)");
        }
    }

    println!();
    println!("=== lifecycle probes ===");
    print_lifecycle_probe(
        "session/resume",
        conn.resume_session(ResumeSessionRequest::new(
            session.session_id.clone(),
            cwd.clone(),
        ))
        .await
        .map(|_| ()),
    );
    print_lifecycle_probe(
        "session/close",
        conn.close_session(CloseSessionRequest::new(session.session_id.clone()))
            .await
            .map(|_| ()),
    );
    print_lifecycle_probe(
        "session/delete",
        conn.delete_session(DeleteSessionRequest::new(session.session_id.clone()))
            .await
            .map(|_| ()),
    );

    conn.shutdown().await?;
    println!("\n=== spike complete ===");
    Ok(())
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::schema::v1::{
        AgentCapabilities, InitializeResponse, NewSessionResponse, SessionCapabilities,
        SessionCloseCapabilities, SessionConfigId, SessionConfigOption,
        SessionConfigOptionCategory, SessionConfigSelectOption, SessionId, SessionListCapabilities,
        SessionResumeCapabilities,
    };
    use agent_client_protocol::schema::ProtocolVersion;
    use spur_acp::{AgentKind, Usage};

    use super::format_probe_report;

    #[test]
    fn report_includes_capabilities_model_option_and_usage() {
        let mut init = InitializeResponse::new(ProtocolVersion::LATEST);
        init.agent_capabilities = AgentCapabilities::new()
            .load_session(true)
            .session_capabilities(
                SessionCapabilities::new()
                    .list(SessionListCapabilities::new())
                    .resume(SessionResumeCapabilities::new())
                    .close(SessionCloseCapabilities::new()),
            );

        let mut session = NewSessionResponse::new(SessionId::new("probe"));
        session.config_options = Some(vec![
            SessionConfigOption::select(
                SessionConfigId::new("reasoning_effort"),
                "Reasoning effort",
                "medium",
                vec![SessionConfigSelectOption::new("medium", "Medium")],
            ),
            SessionConfigOption::select(
                SessionConfigId::new("vendor_model"),
                "Model",
                "fast",
                vec![SessionConfigSelectOption::new("fast", "Fast")],
            )
            .category(SessionConfigOptionCategory::Model),
        ]);

        let usage = Usage::new(120, 70, 50)
            .thought_tokens(7)
            .cached_read_tokens(11)
            .cached_write_tokens(13);

        let report = format_probe_report(&init, &session, Some(&usage), AgentKind::Generic);

        assert!(report.contains("protocol_version:"));
        assert!(report.contains("load_session: yes"));
        assert!(report.contains("sessionCapabilities: list=yes delete=no resume=yes close=yes"));
        assert!(report.contains("config_options: 2"));
        assert!(report.contains("model option: yes id=vendor_model category=Model choices=1"));
        assert!(report.contains("usage: emitted"));
        assert!(report.contains("total_tokens=120"));
        assert!(report.contains("input_tokens=70"));
        assert!(report.contains("output_tokens=50"));
        assert!(report.contains("thought_tokens=Some(7)"));
        assert!(report.contains("cached_read_tokens=Some(11)"));
        assert!(report.contains("cached_write_tokens=Some(13)"));
    }
}
