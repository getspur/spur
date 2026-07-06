use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{backend::TestBackend, Terminal};
use spur_acp::SessionId;
use spur_tui::mentions::{
    CompletionScope, IssueMentionDescriptor, MentionKind, MentionRegistry, WorkerMentionDescriptor,
    CODE_GRAPH_INDEX_ENV,
};
use spur_tui::views::{session_detail::SessionDetailView, View};

static ENV_LOCK: Mutex<()> = Mutex::new(());
const TEST_GRAPH_INDEX_VERSION: &str = "fixture-2026-05-11";

fn test_ctx() -> spur_tui::views::ViewContext<'static> {
    static LINEAGE: std::sync::LazyLock<spur_core::lineage::projection::ExecutorLineage> =
        std::sync::LazyLock::new(spur_core::lineage::projection::ExecutorLineage::new);
    spur_tui::test_support::test_view_ctx(&LINEAGE)
}

fn press(v: &mut SessionDetailView, code: KeyCode) {
    let _ = v.handle_key(KeyEvent::new(code, KeyModifiers::NONE), &test_ctx());
}

fn type_str(v: &mut SessionDetailView, s: &str) {
    for c in s.chars() {
        press(v, KeyCode::Char(c));
    }
}

fn render_text(v: &mut SessionDetailView, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal
        .draw(|f| v.render(f, f.area(), &test_ctx()))
        .expect("render");
    let mut out = String::new();
    let buf = terminal.backend().buffer();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn issue_summary(id: &str, title: &str) -> spur_pm::IssueSummary {
    spur_pm::IssueSummary {
        id: id.to_string(),
        source: spur_pm::PmSource::Beads,
        title: title.to_string(),
        status: "open".to_string(),
        labels: vec!["mentions".to_string()],
        url: format!("https://example.test/{id}"),
        priority: None,
        issue_type: Some("task".to_string()),
        assignee: Some("alice".to_string()),
        description: None,
    }
}

// The legacy JSON artifact read path was removed (078f3c73e); convert the
// JSON fixture shape into a parquet artifact the loader accepts, synthesizing
// the #[serde(skip)] node ids the writer requires.
fn write_graph_fixture(root: &Path, value: serde_json::Value) -> PathBuf {
    let mut artifact: spur_graph::GraphIndexArtifact =
        serde_json::from_value(value).expect("graph fixture json must match artifact shape");
    let n_files = artifact.files.len();
    artifact.file_node_ids = (0..n_files as u64).map(spur_graph::NodeId).collect();
    artifact.symbol_node_ids = (0..artifact.symbols.len() as u64)
        .map(|i| spur_graph::NodeId(n_files as u64 + i))
        .collect();
    spur_graph::write_artifact_parquet(
        &artifact,
        root,
        spur_graph::WriteOptions::default(),
        Vec::new(),
    )
    .expect("write parquet graph fixture");
    root.to_path_buf()
}

fn seed_registry(
    workers: Vec<WorkerMentionDescriptor>,
    issues: &[spur_pm::IssueSummary],
    graph_path: &Path,
) -> MentionRegistry {
    let mut reg = MentionRegistry::for_brain_session(workers).with_code_graph(graph_path);
    reg.set_issue_snapshot(issues.iter().map(IssueMentionDescriptor::from).collect());
    reg
}

#[test]
fn empty_at_shows_sectioned_picker() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let previous_env = std::env::var_os(CODE_GRAPH_INDEX_ENV);

    let tmp = tempfile::tempdir().expect("tempdir");
    for i in 0..1 {
        std::fs::write(
            tmp.path().join(format!("mentions_v2_file_{i:02}.rs")),
            "// fixture",
        )
        .expect("write file fixture");
    }

    // Keep the parquet artifact out of the workspace root so its shard files
    // don't appear in the Files section of the picker.
    let graph_tmp = tempfile::tempdir().expect("graph tempdir");
    let graph_path = write_graph_fixture(
        graph_tmp.path(),
        serde_json::json!({
            "header": { "graph_index_version": TEST_GRAPH_INDEX_VERSION },
            "files": [
                {"stable_file_id": "code-file-00", "file_path": "src/code_file_00.rs"}
            ],
            "symbols": [
                {"stable_symbol_id": "code-symbol-00", "file_path": "src/lib.rs", "byte_range": [0,1], "line_range": [1,1], "entity_name": "CodeSym00", "symbol_kind": "fn", "anchor_hash": "1", "enclosing_scope": "module lib"}
            ]
        }),
    );
    std::env::set_var(CODE_GRAPH_INDEX_ENV, &graph_path);

    let workers: Vec<_> = (0..1)
        .map(|i| WorkerMentionDescriptor {
            name: format!("worker-{i}"),
            kind: spur_acp::AgentKind::Generic,
            cli_identity: format!("worker-{i}"),
            description: Some("fixture".into()),
            tier: Some("generalist".into()),
        })
        .collect();
    let issues: Vec<_> = (0..1)
        .map(|i| issue_summary(&format!("bd-v2-{i}"), &format!("Mention V2 issue {i}")))
        .collect();

    let mut view = SessionDetailView::new(
        SessionId::new(),
        "claude".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
        workers.clone(),
    );
    view.set_issue_snapshot(issues.clone());

    type_str(&mut view, "@");
    assert!(view.completion_active_for_test());

    let rendered = render_text(&mut view, 160, 48);
    let workers_header = "── Workers ──";
    let files_header = "── Files ──";
    let issues_header = "── Issues ──";
    let code_header = "── Code ──";
    assert!(
        rendered.contains(workers_header),
        "picker should render literal Workers section header; rendered=\n{rendered}"
    );
    assert!(
        rendered.contains(files_header),
        "picker should render literal Files section header; rendered=\n{rendered}"
    );
    assert!(
        rendered.contains(issues_header),
        "picker should render literal Issues section header; rendered=\n{rendered}"
    );
    assert!(
        rendered.contains(code_header),
        "picker should render literal Code section header; rendered=\n{rendered}"
    );
    let workers_idx = rendered
        .find(workers_header)
        .expect("workers header in rendered picker");
    let files_idx = rendered
        .find(files_header)
        .expect("files header in rendered picker");
    let issues_idx = rendered
        .find(issues_header)
        .expect("issues header in rendered picker");
    let code_idx = rendered
        .find(code_header)
        .expect("code header in rendered picker");
    assert!(
        workers_idx < files_idx && files_idx < issues_idx && issues_idx < code_idx,
        "expected headers in order Workers -> Files -> Issues -> Code; rendered=\n{rendered}"
    );

    let sid = SessionId::new();
    let mut reg = seed_registry(workers, &issues, &graph_path);
    let hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "", 128);

    let headers: Vec<_> = hits.iter().filter_map(|h| h.section_header).collect();
    assert_eq!(headers, vec!["Workers", "Files", "Issues", "Code"]);

    let workers_idx = hits
        .iter()
        .position(|h| h.section_header == Some("Workers"))
        .unwrap();
    let files_idx = hits
        .iter()
        .position(|h| h.section_header == Some("Files"))
        .unwrap();
    let issues_idx = hits
        .iter()
        .position(|h| h.section_header == Some("Issues"))
        .unwrap();
    let code_idx = hits
        .iter()
        .position(|h| h.section_header == Some("Code"))
        .unwrap();
    assert!(workers_idx < files_idx && files_idx < issues_idx && issues_idx < code_idx);

    let worker_rows = hits[workers_idx + 1..files_idx]
        .iter()
        .filter(|h| h.kind == MentionKind::Worker)
        .count();
    let file_rows = hits[files_idx + 1..issues_idx]
        .iter()
        .filter(|h| h.kind == MentionKind::File)
        .count();
    let issue_rows = hits[issues_idx + 1..code_idx]
        .iter()
        .filter(|h| h.kind == MentionKind::Issue)
        .count();
    let code_rows = hits[code_idx + 1..]
        .iter()
        .filter(|h| matches!(h.kind, MentionKind::CodeFile | MentionKind::CodeSymbol))
        .count();

    assert!(worker_rows <= 4, "worker rows={worker_rows}");
    assert!(file_rows <= 6, "file rows={file_rows}");
    assert!(issue_rows <= 3, "issue rows={issue_rows}");
    assert!(code_rows <= 3, "code rows={code_rows}");

    match previous_env {
        Some(previous) => std::env::set_var(CODE_GRAPH_INDEX_ENV, previous),
        None => std::env::remove_var(CODE_GRAPH_INDEX_ENV),
    }
}

#[test]
fn typed_query_prefers_files_within_window() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let previous_env = std::env::var_os(CODE_GRAPH_INDEX_ENV);

    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("fooe.rs"), "// foo file fixture").expect("write foo file");
    std::fs::write(tmp.path().join("needle-notes.md"), "needle notes fixture")
        .expect("write needle file");

    // Keep the parquet artifact out of the workspace root so its shard files
    // don't appear in the Files section of the picker.
    let graph_tmp = tempfile::tempdir().expect("graph tempdir");
    let graph_path = write_graph_fixture(
        graph_tmp.path(),
        serde_json::json!({
            "header": { "graph_index_version": TEST_GRAPH_INDEX_VERSION },
            "files": [],
            "symbols": [
                {"stable_symbol_id": "symbol-needle", "file_path": "src/needle.rs", "byte_range": [0, 6], "line_range": [1, 1], "entity_name": "needle", "symbol_kind": "struct", "anchor_hash": "999", "enclosing_scope": "module needle"}
            ]
        }),
    );
    std::env::set_var(CODE_GRAPH_INDEX_ENV, &graph_path);

    let workers = vec![WorkerMentionDescriptor {
        name: "food".into(),
        kind: spur_acp::AgentKind::Generic,
        cli_identity: "food".into(),
        description: Some("fixture worker".into()),
        tier: Some("generalist".into()),
    }];

    let mut view = SessionDetailView::new(
        SessionId::new(),
        "claude".into(),
        "brain".into(),
        tmp.path().to_path_buf(),
        spur_tui::test_support::default_agent_config("claude"),
        workers.clone(),
    );

    type_str(&mut view, "@foo");
    assert!(view.completion_active_for_test());
    let rendered = render_text(&mut view, 160, 48);
    assert!(
        rendered.contains("foo"),
        "expected foo query results in picker"
    );

    let sid = SessionId::new();
    let mut reg = seed_registry(workers, &[], &graph_path);
    let foo_hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "foo", 20);
    let foo_file = foo_hits
        .iter()
        .position(|h| h.kind == MentionKind::File)
        .expect("file hit for foo");
    let foo_worker = foo_hits
        .iter()
        .position(|h| h.kind == MentionKind::Worker)
        .expect("worker hit for foo");
    assert!(
        foo_file < foo_worker,
        "expected file to outrank worker inside score window; hits={:?}",
        foo_hits
            .iter()
            .map(|h| (&h.kind, h.display.as_str(), h.uri.as_str()))
            .collect::<Vec<_>>()
    );

    let needle_hits = reg.query(CompletionScope::Session(&sid), tmp.path(), "needle", 20);
    let needle_symbol = needle_hits
        .iter()
        .position(|h| h.kind == MentionKind::CodeSymbol)
        .expect("code symbol hit for needle");
    let needle_file = needle_hits
        .iter()
        .position(|h| h.kind == MentionKind::File)
        .expect("file hit for needle");
    assert!(
        needle_symbol < needle_file,
        "expected higher-bucket code symbol to outrank file outside tier window; hits={:?}",
        needle_hits
            .iter()
            .map(|h| (&h.kind, h.display.as_str(), h.uri.as_str()))
            .collect::<Vec<_>>()
    );

    match previous_env {
        Some(previous) => std::env::set_var(CODE_GRAPH_INDEX_ENV, previous),
        None => std::env::remove_var(CODE_GRAPH_INDEX_ENV),
    }
}
