mod common;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use spur_core::explore::catalog::{Catalog, CatalogEntry, ItemKind};
use spur_core::explore::materialize::{append_materialization_record, MaterializationRecord};
use spur_core::explore::pool::{item_from_entry, pool_dir, GateRecord, Manifest};
use spur_core::{ExecutorLineage, PlanProjectionStore, SessionSynopsisProjection};
use spur_tui::views::explore::ExploreBrowserView;
use spur_tui::views::ViewContext;

static BRAIN_STATUS: spur_tui::app::BrainStatus = spur_tui::app::BrainStatus::Idle;
static SYNOPSIS: std::sync::LazyLock<SessionSynopsisProjection> =
    std::sync::LazyLock::new(SessionSynopsisProjection::new);

fn view_ctx<'a>(lineage: &'a ExecutorLineage, plans: &'a PlanProjectionStore) -> ViewContext<'a> {
    ViewContext {
        lineage,
        plan_projection: plans,
        synopsis: &SYNOPSIS,
        brain_status: &BRAIN_STATUS,
        flag_summary: None,
        tombstone: None,
        transient_hint_override: None,
        notebook_ready: false,
        theme: spur_tui::theme::fallback_theme(),
    }
}

fn golden_path(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(name)
}

fn check_or_update(actual: &str, golden_name: &str) {
    let path = golden_path(golden_name);
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(&path, actual).expect("write golden file");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "golden file not found: {}; run with UPDATE_GOLDEN=1 to create it",
            path.display()
        )
    });
    assert_eq!(
        actual, expected,
        "golden mismatch for {}; re-record with UPDATE_GOLDEN=1",
        golden_name
    );
}

fn entry(kind: ItemKind, name: &str, rel_path: &str, description: &str) -> CatalogEntry {
    CatalogEntry {
        kind,
        name: name.into(),
        source: "getspur/ecosystem".into(),
        rel_path: rel_path.into(),
        pinned_commit: "0123456789abcdef".into(),
        description: description.into(),
        license: Some("MIT".into()),
        content_sha256: format!("{name}-sha"),
    }
}

fn fixture_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("temp repo");
    let skill = entry(
        ItemKind::Skill,
        "review-helper",
        "skills/review-helper",
        "Tightens code review with focused risk checks.",
    );
    let agent = entry(
        ItemKind::Agent,
        "release-captain",
        "agents/release-captain.md",
        "Prepares release notes and verifies launch checklists.",
    );

    Catalog {
        synced_at_epoch: None,
        entries: vec![skill.clone(), agent],
    }
    .save(repo.path())
    .expect("save catalog");

    Manifest {
        sources: Vec::new(),
        items: vec![item_from_entry(
            &skill,
            GateRecord {
                verdict: "clean".into(),
                justification: None,
                decided_at_epoch: Some(1_700_000_000),
            },
        )],
    }
    .save(repo.path())
    .expect("save manifest");

    let skill_dir = pool_dir(
        repo.path(),
        &skill.source,
        &skill.name,
        &skill.pinned_commit,
    );
    std::fs::create_dir_all(&skill_dir).expect("create skill pool dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "# Review Helper\n\nUse focused risk checks before approving code.",
    )
    .expect("write vendored skill body");

    repo
}

fn fixture_repo_with_status_warning() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("temp repo");
    let mut skill = entry(
        ItemKind::Skill,
        "review-helper",
        "skills/review-helper",
        "Tightens code review with focused risk checks.",
    );
    let stale = entry(
        ItemKind::Skill,
        "stale-helper",
        "skills/stale-helper",
        "Demonstrates stale pool status findings.",
    );

    let skill_dir = pool_dir(
        repo.path(),
        &skill.source,
        &skill.name,
        &skill.pinned_commit,
    );
    std::fs::create_dir_all(&skill_dir).expect("create skill pool dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "# Review Helper\n\nUse focused risk checks before approving code.",
    )
    .expect("write vendored skill body");
    skill.content_sha256 = spur_core::explore::content_hash(&skill_dir).expect("hash skill dir");

    Catalog {
        synced_at_epoch: None,
        entries: vec![skill.clone(), stale.clone()],
    }
    .save(repo.path())
    .expect("save catalog");

    let gate = GateRecord {
        verdict: "clean".into(),
        justification: None,
        decided_at_epoch: Some(1_700_000_000),
    };
    Manifest {
        sources: Vec::new(),
        items: vec![
            item_from_entry(&skill, gate.clone()),
            item_from_entry(&stale, gate),
        ],
    }
    .save(repo.path())
    .expect("save manifest");

    repo
}

fn fixture_repo_with_materializations() -> tempfile::TempDir {
    let repo = fixture_repo();
    append_materialization_record(
        repo.path(),
        &MaterializationRecord {
            recorded_at_epoch: 1_700_000_000,
            delegation_id: "delegation-old".into(),
            agent: "claude".into(),
            worktree: "/tmp/old".into(),
            items: vec!["older-helper".into()],
        },
    )
    .expect("append old materialization");
    append_materialization_record(
        repo.path(),
        &MaterializationRecord {
            recorded_at_epoch: 1_700_000_060,
            delegation_id: "delegation-new".into(),
            agent: "codex".into(),
            worktree: "/tmp/new".into(),
            items: vec!["review-helper".into(), "deploy-helper".into()],
        },
    )
    .expect("append new materialization");
    repo
}

fn send_key(view: &mut ExploreBrowserView, code: KeyCode) {
    view.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn render_to_string(view: &mut ExploreBrowserView) -> String {
    let lineage = ExecutorLineage::new();
    let plans = PlanProjectionStore::default();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");

    terminal
        .draw(|frame| view.render(frame, frame.area(), &view_ctx(&lineage, &plans)))
        .expect("draw explore browser");

    common::buffer_text(terminal.backend().buffer())
}

#[test]
fn explore_browser_browse_renders_golden() {
    let repo = fixture_repo();
    let mut view = ExploreBrowserView::new(repo.path().to_path_buf());

    let actual = render_to_string(&mut view);

    assert!(actual.contains("Explore"));
    assert!(actual.contains("never synced"));
    assert!(actual.contains("review-helper"));
    assert!(actual.contains("in pool"));
    check_or_update(&actual, "explore_browser_browse.txt");
}

#[test]
fn explore_browser_manage_pool_renders_golden() {
    let repo = fixture_repo_with_status_warning();
    let mut view = ExploreBrowserView::new(repo.path().to_path_buf());
    send_key(&mut view, KeyCode::Char('m'));

    let actual = render_to_string(&mut view);

    assert!(actual.contains("Manage"));
    assert!(actual.contains("Pool"));
    assert!(actual.contains("review-helper"));
    assert!(actual.contains("stale-helper"));
    assert!(actual.contains("missing body"));
    check_or_update(&actual, "explore_browser_manage_pool.txt");
}

#[test]
fn explore_browser_manage_last_materialization_renders_newest_first_golden() {
    let repo = fixture_repo_with_materializations();
    let mut view = ExploreBrowserView::new(repo.path().to_path_buf());
    send_key(&mut view, KeyCode::Char('m'));
    send_key(&mut view, KeyCode::Char('l'));

    let actual = render_to_string(&mut view);

    let newest = actual
        .find("delegation-new")
        .expect("newest materialization rendered");
    let oldest = actual
        .find("delegation-old")
        .expect("oldest materialization rendered");
    assert!(
        newest < oldest,
        "materializations should render newest first"
    );
    assert!(actual.contains("2 skills: review-helper, deploy-helper"));
    check_or_update(&actual, "explore_browser_manage_lastmat.txt");
}
