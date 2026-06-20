//! Integration tests for `Orchestrator::init_agents()`.
//!
//! Uses a temp directory as an isolated $PATH so these tests don't
//! depend on what's installed on the developer's machine. These tests
//! pin the Spec 3 refactor (hand-rolled per-agent struct → embedded
//! seed_agents.toml).

#![cfg(unix)] // stub_binary uses Unix permissions. Windows not supported.

use spur_acp::config::SpurConfig;
use spur_core::Orchestrator;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Mutex;
use tempfile::TempDir;

// Serialize PATH-mutating tests to avoid cross-test env pollution.
static PATH_LOCK: Mutex<()> = Mutex::new(());

/// Create an executable stub at `<dir>/<name>` that exits 0.
fn stub_binary(dir: &std::path::Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn init_agents_finds_only_stubs_on_path() {
    let tmp = TempDir::new().unwrap();
    stub_binary(tmp.path(), "kiro-cli");
    // Deliberately NOT stubbing claude/codex/npx/gemini.

    let result = {
        let _guard = PATH_LOCK.lock().unwrap();
        let prev_path = std::env::var_os("PATH");
        std::env::set_var("PATH", tmp.path());
        let r = {
            let mut orch =
                Orchestrator::new(tmp.path().into(), SpurConfig::default(), None).unwrap();
            orch.init_agents().await.unwrap()
        };
        if let Some(p) = prev_path {
            std::env::set_var("PATH", p);
        }
        r
    };

    assert_eq!(
        result,
        vec!["kiro".to_string()],
        "only kiro-cli is stubbed, only kiro should be found"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn init_agents_with_empty_path_returns_empty() {
    let tmp = TempDir::new().unwrap();
    // tmp is empty (no stubs created).

    let result = {
        let _guard = PATH_LOCK.lock().unwrap();
        let prev_path = std::env::var_os("PATH");
        std::env::set_var("PATH", tmp.path());
        let r = {
            let mut orch =
                Orchestrator::new(tmp.path().into(), SpurConfig::default(), None).unwrap();
            orch.init_agents().await.unwrap()
        };
        if let Some(p) = prev_path {
            std::env::set_var("PATH", p);
        }
        r
    };

    assert!(
        result.is_empty(),
        "nothing on PATH should find no agents, got {result:?}"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn init_agents_registers_full_spec12_config() {
    // Proves seed agents carry commands/permissions/display blocks,
    // not just a handful of fields like the pre-Spec-3 hardcoded table.
    // This is the key Spec 3 win — spur init now produces
    // config-complete entries.
    let tmp = TempDir::new().unwrap();
    stub_binary(tmp.path(), "kiro-cli");

    let registered = {
        let _guard = PATH_LOCK.lock().unwrap();
        let prev_path = std::env::var_os("PATH");
        std::env::set_var("PATH", tmp.path());
        let r = {
            let mut orch =
                Orchestrator::new(tmp.path().into(), SpurConfig::default(), None).unwrap();
            orch.init_agents().await.unwrap();
            orch.registry
                .list()
                .into_iter()
                .cloned()
                .collect::<Vec<_>>()
        };
        if let Some(p) = prev_path {
            std::env::set_var("PATH", p);
        }
        r
    };

    let kiro = registered
        .iter()
        .find(|a| a.name == "kiro")
        .expect("kiro should be registered");
    assert_eq!(kiro.commands.dispatch, spur_acp::DispatchKind::PromptText);
    assert!(
        kiro.commands.exec_method.is_none(),
        "prompt_text dispatch should not carry an exec_method"
    );
    assert!(
        !kiro.commands.ingest.is_empty(),
        "kiro should still ingest commands/available notifications"
    );
    assert!(
        kiro.commands.response.is_empty(),
        "prompt_text dispatch has no vendor-exec response to render"
    );
    // Bypass args declared but skip = false → NOT applied (safety-by-default).
    // effective_permissions() returns the nested block.
    assert_eq!(kiro.effective_permissions().args, vec!["--trust-all-tools"]);
    assert!(
        !kiro.effective_permissions().skip,
        "seed template declares bypass mechanism but keeps skip=false for safety"
    );
}
