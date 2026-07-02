//! Skip-permissions diagnostic probe.
//!
//! A permanent diagnostic that exercises the per-agent `skip_permissions`
//! levers against a live agent and reports how many ACP `request_permission`
//! calls round-tripped. Used to:
//!
//!   - Verify a new agent's bypass claims before adding it to the
//!     supported matrix (do `--trust-all-tools` / `bypassPermissions`
//!     actually suppress permission calls?).
//!   - Catch regressions when upgrading a pinned agent version.
//!
//! Covers two known-good agents today:
//!
//!   C1 (kiro-cli) — `kiro-cli acp --trust-all-tools` should suppress
//!       all ACP `request_permission` calls.
//!   C2 (claude-code-acp) — ACP `set_session_mode("bypassPermissions")`
//!       post-`new_session` should suppress all `request_permission`
//!       calls in practice.
//!
//! Design reference:
//!   docs/superpowers/specs/2026-04-14-spur-acp-skip-permissions-design.md
//!
//! Run:
//!   cargo run -p spur-acp --example skip_perm_spike -- <agent> <mode> [cwd]
//!
//! Where:
//!   agent ∈ { claude-code-acp, kiro }
//!   mode  ∈ { off, args, session, both }
//!   cwd   defaults to the current working directory
//!
//! Each run prints one summary line:
//!   agent=<…> mode=<…> permission_calls=<N> notifs=<N> took=<…>ms outcome=<ok|err>

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use agent_client_protocol::schema::v1::{
    ContentBlock, InitializeRequest, PermissionOptionId, PromptRequest, SetSessionModeRequest,
    TextContent,
};
use agent_client_protocol::schema::ProtocolVersion;
use spur_acp::connection::{AgentConnection, NativeAcpConnection};
use spur_acp::types::{PermissionRequest, PermissionResponse};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy)]
enum Mode {
    Off,
    Args,
    Session,
    Both,
}

impl Mode {
    fn parse(s: &str) -> anyhow::Result<Self> {
        Ok(match s {
            "off" => Mode::Off,
            "args" => Mode::Args,
            "session" => Mode::Session,
            "both" => Mode::Both,
            other => anyhow::bail!("unknown mode '{other}' (want off|args|session|both)"),
        })
    }
    fn wants_args(&self) -> bool {
        matches!(self, Mode::Args | Mode::Both)
    }
    fn wants_session(&self) -> bool {
        matches!(self, Mode::Session | Mode::Both)
    }
}

struct AgentSpec {
    label: &'static str,
    command: &'static str,
    base_args: Vec<String>,
    skip_args: Vec<String>,
    supports_session_mode: bool,
}

fn agent_spec(name: &str) -> anyhow::Result<AgentSpec> {
    Ok(match name {
        "claude-code-acp" => AgentSpec {
            label: "claude-code-acp",
            command: "npx",
            base_args: vec![
                "--yes".into(),
                "@agentclientprotocol/claude-agent-acp@0.26.0".into(),
            ],
            // claude-agent-acp takes no CLI flags; bypass goes through
            // session mode. `args` mode is a no-op placeholder kept for
            // matrix symmetry.
            skip_args: vec![],
            supports_session_mode: true,
        },
        "kiro" => AgentSpec {
            label: "kiro",
            command: "kiro-cli",
            base_args: vec!["acp".into()],
            skip_args: vec!["--trust-all-tools".into()],
            supports_session_mode: false,
        },
        other => anyhow::bail!("unknown agent '{other}' (want claude-code-acp|kiro)"),
    })
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("spur_acp=warn,skip_perm_spike=info")
        .init();

    let agent_arg = std::env::args().nth(1).unwrap_or_else(|| "--help".into());
    if agent_arg == "--help" || agent_arg == "-h" {
        eprintln!(
            "usage: cargo run -p spur-acp --example skip_perm_spike -- \
             <claude-code-acp|kiro> <off|args|session|both> [cwd]"
        );
        return Ok(());
    }
    let mode = Mode::parse(
        &std::env::args()
            .nth(2)
            .ok_or_else(|| anyhow::anyhow!("missing mode arg"))?,
    )?;
    let cwd: PathBuf = std::env::args()
        .nth(3)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().expect("cwd"));

    let spec = agent_spec(&agent_arg)?;
    let started = Instant::now();

    let outcome = run_probe(&spec, mode, &cwd).await;

    match outcome {
        Ok(report) => {
            println!(
                "agent={} mode={:?} permission_calls={} notifs={} took={}ms outcome=ok",
                spec.label,
                mode,
                report.permission_calls,
                report.notifs,
                started.elapsed().as_millis()
            );
        }
        Err(e) => {
            println!(
                "agent={} mode={:?} permission_calls=? notifs=? took={}ms outcome=err: {e:#}",
                spec.label,
                mode,
                started.elapsed().as_millis()
            );
        }
    }

    Ok(())
}

struct ProbeReport {
    permission_calls: u32,
    notifs: u32,
}

async fn run_probe(spec: &AgentSpec, mode: Mode, cwd: &Path) -> anyhow::Result<ProbeReport> {
    // Assemble spawn args.
    let mut spawn_args = spec.base_args.clone();
    if mode.wants_args() {
        spawn_args.extend(spec.skip_args.iter().cloned());
    }

    // Wire a counting permission channel. Every incoming PermissionRequest is
    // counted then auto-approved with options.first() (matches spur-acp's
    // existing auto_approve semantics).
    let (perm_tx, mut perm_rx) = mpsc::unbounded_channel::<PermissionRequest>();
    let perm_counter = Arc::new(AtomicU32::new(0));
    let counter = perm_counter.clone();
    let perm_task = tokio::spawn(async move {
        while let Some(req) = perm_rx.recv().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let option_id: PermissionOptionId = req
                .args
                .options
                .first()
                .map(|o| o.option_id.clone())
                .unwrap_or_else(|| PermissionOptionId::new("allow"));
            eprintln!(
                "[probe] permission request: tool={} options={:?} -> approving {:?}",
                req.args.tool_call.tool_call_id,
                req.args
                    .options
                    .iter()
                    .map(|o| o.option_id.0.clone())
                    .collect::<Vec<_>>(),
                option_id.0
            );
            let _ = req.reply_tx.send(PermissionResponse {
                option_id: option_id.0.to_string(),
            });
        }
    });

    let mut conn = NativeAcpConnection::new(
        format!("{}-spike", spec.label),
        spec.command,
        spawn_args.clone(),
        Some(perm_tx.clone()),
    );

    eprintln!("[probe] spawn: {} {:?}", spec.command, spawn_args);

    conn.initialize(InitializeRequest::new(ProtocolVersion::LATEST))
        .await?;

    // Subscribe BEFORE new_session so any notifications the agent emits
    // during session setup (available_commands_update etc.) land on a
    // live receiver. broadcast::send drops items when there are no
    // receivers, so late subscribers miss early notifications.
    let notif_rx = conn.subscribe_session_notifications();

    let session = conn.new_session(cwd.to_path_buf(), vec![]).await?;
    let session_id = session.session_id.clone();
    eprintln!("[probe] new_session: {}", session_id);

    if mode.wants_session() {
        if !spec.supports_session_mode {
            eprintln!(
                "[probe] WARN: mode requests session-mode bypass but agent '{}' \
                 is not declared to support it; calling anyway",
                spec.label
            );
        }
        let req = SetSessionModeRequest::new(session_id.clone(), "bypassPermissions");
        match conn.set_session_mode(req).await {
            Ok(_) => eprintln!("[probe] set_session_mode(bypassPermissions) ok"),
            Err(e) => eprintln!("[probe] set_session_mode(bypassPermissions) err: {e}"),
        }
    }

    // Prompt guaranteed to trigger at least one file-write tool call under a
    // non-bypass agent. The filename is per-run so repeated invocations don't
    // trip the agent's "file already exists" reasoning.
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let probe_path = format!("/tmp/spur-skip-probe-{ts}.txt");
    let prompt_text = format!(
        "Write exactly the two characters \"ok\" (no quotes, no newline) \
         to the file {probe_path}. Do not ask me for confirmation. \
         Use whichever file-writing tool you have available. \
         After the write, reply with just the word DONE and stop."
    );

    let prompt_req = PromptRequest::new(
        session_id.clone(),
        vec![ContentBlock::Text(TextContent::new(prompt_text))],
    );

    // notif_rx was subscribed before new_session so we already hold a
    // receiver that captured any pre-prompt notifications.
    let _stream = conn.prompt(prompt_req).await?;

    // After prompt returns, drain buffered broadcast items until idle or
    // the 90s safety cap.
    let mut notifs: u32 = 0;
    if let Some(mut rx) = notif_rx {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                eprintln!("[probe] broadcast drain TIMED OUT after 90s ({notifs} notifs)");
                break;
            }
            // Use a short per-recv timeout so we exit soon after the agent
            // goes quiet instead of waiting the full 90s.
            let idle_cap = std::cmp::min(remaining, Duration::from_millis(500));
            match tokio::time::timeout(idle_cap, rx.recv()).await {
                Ok(Ok(_notif)) => {
                    notifs += 1;
                    if notifs > 5000 {
                        break;
                    }
                }
                Ok(Err(_)) => break, // broadcast closed
                Err(_) => {
                    // idle for 500ms — treat as done.
                    eprintln!("[probe] broadcast drained ({notifs} notifs)");
                    break;
                }
            }
        }
    } else {
        eprintln!("[probe] transport does not expose a broadcast subscriber; notifs=0");
    }

    // Sanity: did the file get written? Report but don't fail on it — we
    // care about permission counts, not file contents.
    let wrote = std::fs::metadata(&probe_path).is_ok();
    eprintln!("[probe] file_written={wrote} path={probe_path}");

    let report = ProbeReport {
        permission_calls: perm_counter.load(Ordering::SeqCst),
        notifs,
    };

    // Best-effort shutdown with a hard deadline. Agents occasionally take a
    // while to drain child processes; a bounded wait keeps the matrix loop
    // moving. Any orphan subprocess gets reaped by the NativeAcpConnection
    // Drop impl (killpg on SIGKILL).
    let _ = tokio::time::timeout(Duration::from_secs(3), conn.shutdown()).await;
    drop(perm_tx);
    let _ = tokio::time::timeout(Duration::from_secs(1), perm_task).await;

    Ok(report)
}
