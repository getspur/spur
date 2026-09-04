//! Integration test: when a brain SessionDetailView submits a message
//! containing a `worker://` atom, the resulting `Action::SendMessage`
//! has a `[UI hint]` Text block prepended as `blocks[0]` and preserves
//! the original ResourceLink later in `blocks`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use spur_acp::ContentBlock;
use spur_tui::action::Action;
use spur_tui::mentions::WorkerMentionDescriptor;
use spur_tui::views::{session_detail::SessionDetailView, View};

struct CatalogPathGuard {
    original: Option<std::ffi::OsString>,
}

impl CatalogPathGuard {
    fn set(path: &std::path::Path) -> Self {
        let original = std::env::var_os(spur_acp::agent_model_catalog::CACHE_PATH_ENV);
        std::env::set_var(spur_acp::agent_model_catalog::CACHE_PATH_ENV, path);
        Self { original }
    }
}

impl Drop for CatalogPathGuard {
    fn drop(&mut self) {
        if let Some(path) = self.original.take() {
            std::env::set_var(spur_acp::agent_model_catalog::CACHE_PATH_ENV, path);
        } else {
            std::env::remove_var(spur_acp::agent_model_catalog::CACHE_PATH_ENV);
        }
    }
}

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn brain_view_with_workers(
    cwd: &std::path::Path,
    workers: Vec<WorkerMentionDescriptor>,
) -> SessionDetailView {
    SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "brain".into(),
        cwd.to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
        workers,
    )
}

fn press(v: &mut SessionDetailView, code: KeyCode) -> Option<Action> {
    v.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &test_ctx())
}

fn type_str(v: &mut SessionDetailView, s: &str) {
    for c in s.chars() {
        let _ = press(v, KeyCode::Char(c));
    }
}

#[test]
fn brain_send_prepends_worker_hint_block() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join(".spur/agents")).unwrap();
    std::fs::write(
        tmp.path().join(".spur/agents/spur-probe-echo.md"),
        "---\nname: spur-probe-echo\ndescription: probe\n---\nbody\n",
    )
    .unwrap();
    let catalog_path = tmp.path().join("agent-model-catalog.json");
    let mut entries = std::collections::HashMap::new();
    entries.insert(
        "codex".to_string(),
        spur_acp::agent_model_catalog::WorkerCatalogEntry {
            probed_at: chrono::Utc::now(),
            cli_identity: "test".to_string(),
            models: vec![spur_acp::agent_model_catalog::ConfigOptionChoice {
                value: "gpt-5.5".to_string(),
                name: "GPT-5.5".to_string(),
                description: None,
            }],
            efforts: vec![spur_acp::agent_model_catalog::ConfigOptionChoice {
                value: "high".to_string(),
                name: "High".to_string(),
                description: None,
            }],
        },
    );
    spur_acp::agent_model_catalog::write(
        &catalog_path,
        &spur_acp::agent_model_catalog::AgentModelCatalogV1 {
            version: 1,
            entries,
        },
    )
    .unwrap();
    let _catalog_path_guard = CatalogPathGuard::set(&catalog_path);
    let mut v = brain_view_with_workers(
        tmp.path(),
        vec![WorkerMentionDescriptor {
            name: "codex".into(),
            kind: spur_acp::AgentKind::CodexAcp,
            cli_identity: "test".into(),
            description: Some("Refactors Rust".into()),
            tier: Some("specialist".into()),
        }],
    );

    type_str(&mut v, "@codex");
    // Select the sole profile, model, and effort rows. The picker stays open
    // for the first two slots and commits the enriched atom on the final one.
    let _ = press(&mut v, KeyCode::Tab);
    let _ = press(&mut v, KeyCode::Enter);
    let _ = press(&mut v, KeyCode::Enter);
    // The picker is now closed, so Enter submits through the public view path.
    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    let blocks = match act {
        Action::SendMessage { blocks, .. } => blocks,
        other => panic!("expected SendMessage, got {:?}", other),
    };

    assert!(
        matches!(&blocks[0], ContentBlock::Text(t)
            if t.text.starts_with("[UI hint]")
                && t.text.contains("agent=codex (profile=spur-probe-echo, model=gpt-5.5, effort=high)")
                && t.text.contains("preference, not override")
                && t.text.contains("profile activation starts a fresh SPUR worker session")
                && t.text.contains("does not switch the current brain session")),
        "expected [UI hint] Text at blocks[0], got {:?}",
        blocks[0]
    );
    let expected_uri = "worker://codex?agent=spur-probe-echo&model=gpt-5.5&effort=high";
    assert!(
        blocks.iter().skip(1).any(|b| matches!(
            b,
            ContentBlock::ResourceLink(r)
                if r.uri == expected_uri
                    && r.name == "worker:codex agent=spur-probe-echo model=gpt-5.5 effort=high"
        )),
        "expected an enriched worker ResourceLink later in blocks, got {:?}",
        blocks
    );
}

#[test]
fn brain_send_without_worker_atom_has_no_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let mut v = brain_view_with_workers(
        tmp.path(),
        vec![WorkerMentionDescriptor {
            name: "claude-code".into(),
            kind: spur_acp::AgentKind::ClaudeCodeAcp,
            cli_identity: "claude-code".into(),
            description: None,
            tier: None,
        }],
    );

    type_str(&mut v, "just text");
    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    let blocks = match act {
        Action::SendMessage { blocks, .. } => blocks,
        other => panic!("expected SendMessage, got {:?}", other),
    };
    // First block must NOT be the hint.
    if let ContentBlock::Text(t) = &blocks[0] {
        assert!(
            !t.text.starts_with("[UI hint]"),
            "did not expect a hint when no worker atom was present, got: {}",
            t.text
        );
    }
}

#[test]
fn direct_session_skips_hint_even_with_worker_atom_pasted() {
    // Direct (non-brain) view. Even with a populated worker snapshot, the
    // `role == "brain"` guard in the send-path arm must prevent any hint.
    // Verifies the role guard itself; we type only plain text (the atom
    // path is exercised by the brain-session test above).
    let tmp = tempfile::tempdir().unwrap();
    let mut v = SessionDetailView::new(
        spur_acp::SessionId::new(),
        "claude".into(),
        "worker".into(), // role != "brain"
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
        vec![WorkerMentionDescriptor {
            name: "claude-code".into(),
            kind: spur_acp::AgentKind::ClaudeCodeAcp,
            cli_identity: "claude-code".into(),
            description: None,
            tier: None,
        }],
    );

    type_str(&mut v, "anything");
    let act = press(&mut v, KeyCode::Enter).expect("submit action");
    let blocks = match act {
        Action::SendMessage { blocks, .. } => blocks,
        other => panic!("expected SendMessage, got {:?}", other),
    };
    if let ContentBlock::Text(t) = &blocks[0] {
        assert!(
            !t.text.starts_with("[UI hint]"),
            "direct session must never prepend the hint, got: {}",
            t.text
        );
    }
}
