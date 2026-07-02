use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_core::server::{community_feature_gate, DetachedContinuationCtx};
use spur_core::McpCallbackServer;
use spur_graph::store::lance_sections::{
    write_sections_dataset, write_sections_dataset_skipping_embeddings,
};
use spur_graph::{
    artifact_from_facts, build_facts, write_artifact_parquet, write_current_pointer, WriteOptions,
};

static CWD_LOCK: Mutex<()> = Mutex::new(());

struct CwdGuard {
    original: PathBuf,
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
    }
}

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

fn enter_dir(path: &Path) -> CwdGuard {
    let original = std::env::current_dir().expect("current dir");
    std::env::set_current_dir(path).expect("set current dir");
    CwdGuard { original }
}

fn continuation_ctx() -> DetachedContinuationCtx {
    DetachedContinuationCtx {
        on_complete: Arc::new(|_, _| Box::pin(async {})),
    }
}

fn test_server() -> McpCallbackServer {
    let session_id = BrainSessionId::new(SessionId("brain-doc-navigate-e2e".into()));
    let (server, _channel) = McpCallbackServer::new(
        Some(&session_id),
        None,
        None,
        continuation_ctx(),
        Arc::new(spur_blob_store::MemoryOutcomeStore::new()),
        community_feature_gate(),
    );
    server
}

fn git(worktree: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(worktree)
        .output()
        .unwrap_or_else(|error| panic!("run git {}: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_doc_fixture(worktree: &Path) {
    std::fs::create_dir_all(worktree.join("docs")).expect("create docs dir");
    std::fs::write(
        worktree.join("docs/stage1.md"),
        "# Stage 1\n\n\
         Intro text.\n\n\
         ## markdown.rs Section Body Span Fix\n\n\
         The markdown.rs Section that introduced the body-span fix widened \
         emit_sections body spans so Lance body_text includes the complete \
         section body for navigation snippets.\n\n\
         ### Child Detail\n\n\
         Child body follows the parent in source order.\n\n\
         ## Later Section\n\n\
         Later sibling body.\n",
    )
    .expect("write markdown fixture");
    std::fs::create_dir_all(worktree.join(".git")).expect("create git marker");
}

fn write_graph_with_sections(worktree: &Path) {
    let facts = build_facts(worktree, None).expect("build graph facts").0;
    let artifact = artifact_from_facts(&facts, worktree).expect("build graph artifact");
    let artifact_base = worktree.join(".spur/graph");
    let written = write_artifact_parquet(
        &artifact,
        &artifact_base,
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write graph artifact");
    write_sections_dataset(&artifact, worktree, &written).expect("write sections sidecar");
    write_current_pointer(worktree, &written).expect("write CURRENT pointer");
}

fn write_graph_with_sections_skipping_embeddings(worktree: &Path) {
    let facts = build_facts(worktree, None).expect("build graph facts").0;
    let artifact = artifact_from_facts(&facts, worktree).expect("build graph artifact");
    let artifact_base = worktree.join(".spur/graph");
    let written = write_artifact_parquet(
        &artifact,
        &artifact_base,
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write graph artifact");
    write_sections_dataset_skipping_embeddings(&artifact, worktree, &written)
        .expect("write sections sidecar");
    write_current_pointer(worktree, &written).expect("write CURRENT pointer");
}

fn write_graph_without_sections(worktree: &Path) {
    let facts = build_facts(worktree, None).expect("build graph facts").0;
    let artifact = artifact_from_facts(&facts, worktree).expect("build graph artifact");
    let artifact_base = worktree.join(".spur/graph");
    let written = write_artifact_parquet(
        &artifact,
        &artifact_base,
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write graph artifact");
    write_current_pointer(worktree, &written).expect("write CURRENT pointer");
}

fn write_overlay_docs(worktree: &Path) {
    std::fs::create_dir_all(worktree.join("docs")).expect("create docs dir");
    std::fs::write(
        worktree.join("docs/stable.md"),
        "# Stable\n\nstableoverlayneedle unchanged base body.\n",
    )
    .expect("write stable markdown");
    std::fs::write(
        worktree.join("docs/changed.md"),
        "# Changed\n\noldoverlayneedle body before edit.\n",
    )
    .expect("write changed markdown");
    std::fs::write(
        worktree.join("docs/deleted.md"),
        "# Deleted\n\ndeletedoverlayneedle body before removal.\n",
    )
    .expect("write deleted markdown");
}

fn create_worker_from_main(main: &Path, name: &str) -> PathBuf {
    git(main, &["branch", name]);
    let worker = main.parent().expect("tempdir").join(name);
    git(
        main,
        &[
            "worktree",
            "add",
            worker.to_str().expect("worker path"),
            name,
        ],
    );
    worker
}

async fn call_tool(server: &McpCallbackServer, tool: &str, arguments: Value) -> Value {
    server.__test_call_tool(tool, arguments).await
}

fn tool_body(response: Value) -> Value {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("successful tool response with text content: {response}"));
    serde_json::from_str(text).expect("tool text is JSON")
}

#[tokio::test]
async fn doc_navigate_returns_fts_hits_with_score_and_lede() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let worktree = tempdir.path().join("repo");
    std::fs::create_dir_all(&worktree).expect("create worktree");
    git(&worktree, &["init", "-q"]);
    git(&worktree, &["config", "user.email", "test@spur"]);
    git(&worktree, &["config", "user.name", "SPUR Test"]);
    write_doc_fixture(&worktree);
    git(&worktree, &["add", "."]);
    git(&worktree, &["commit", "-m", "fixture"]);
    write_graph_with_sections(&worktree);

    let _cwd = enter_dir(&worktree);
    let response = call_tool(
        &test_server(),
        "doc_navigate",
        json!({
            "query": "emit_sections body span widen",
            "k": 5
        }),
    )
    .await;
    let body = tool_body(response);
    let hits = body["hits"].as_array().expect("hits array");
    let hit = hits
        .iter()
        .find(|hit| {
            hit["qualified_name"]
                .as_str()
                .is_some_and(|name| name.contains("markdown.rs Section"))
        })
        .unwrap_or_else(|| panic!("expected markdown.rs Section hit in {hits:#?}"));

    assert!(
        hit["score"].as_f64().is_some_and(|score| score > 0.0),
        "FTS hit should include a positive BM25 score: {hit:#?}"
    );
    assert!(
        hit["lede"]
            .as_str()
            .is_some_and(|lede| lede.contains("emit_sections body spans")),
        "lede should include body_text snippet: {hit:#?}"
    );
    assert!(
        hit["stable_symbol_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "stable_symbol_id should be surfaced: {hit:#?}"
    );
}

#[tokio::test]
async fn doc_navigate_overlay_unchanged_markdown_serves_base_without_sidecar_write() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let main = tempdir.path().join("main");
    std::fs::create_dir_all(&main).expect("create main worktree");
    git(&main, &["init", "-q"]);
    git(&main, &["config", "user.email", "test@spur"]);
    git(&main, &["config", "user.name", "SPUR Test"]);
    write_overlay_docs(&main);
    git(&main, &["add", "."]);
    git(&main, &["commit", "-m", "fixture"]);
    write_graph_with_sections_skipping_embeddings(&main);
    let worker = create_worker_from_main(&main, "worker-unchanged");

    let _fail_on_overlay_write = EnvGuard::set("SPUR_GRAPH_TEST_FAIL_SECTION_SIDECAR", "1");
    let _cwd = enter_dir(&worker);
    let response = call_tool(
        &test_server(),
        "doc_navigate",
        json!({
            "query": "stableoverlayneedle",
            "k": 5
        }),
    )
    .await;
    let body = tool_body(response);
    let hits = body["hits"].as_array().expect("hits array");

    assert!(
        hits.iter().any(|hit| hit["file_path"] == "docs/stable.md"),
        "unchanged worker should serve the base sidecar without writing an overlay: {hits:#?}"
    );
}

#[tokio::test]
async fn doc_navigate_overlay_delta_reflects_edited_and_deleted_markdown() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let main = tempdir.path().join("main");
    std::fs::create_dir_all(&main).expect("create main worktree");
    git(&main, &["init", "-q"]);
    git(&main, &["config", "user.email", "test@spur"]);
    git(&main, &["config", "user.name", "SPUR Test"]);
    write_overlay_docs(&main);
    git(&main, &["add", "."]);
    git(&main, &["commit", "-m", "fixture"]);
    write_graph_with_sections_skipping_embeddings(&main);
    let worker = create_worker_from_main(&main, "worker-delta");
    std::fs::write(
        worker.join("docs/changed.md"),
        "# Changed\n\nnewoverlayneedle body after edit.\n",
    )
    .expect("edit changed markdown");
    std::fs::remove_file(worker.join("docs/deleted.md")).expect("remove deleted markdown");

    let _cwd = enter_dir(&worker);
    let response = call_tool(
        &test_server(),
        "doc_navigate",
        json!({
            "query": "newoverlayneedle",
            "k": 5
        }),
    )
    .await;
    let body = tool_body(response);
    let hits = body["hits"].as_array().expect("new hits array");
    assert!(
        hits.iter().any(|hit| hit["file_path"] == "docs/changed.md"),
        "edited markdown should be searchable through the overlay delta: {hits:#?}"
    );

    for stale_query in ["oldoverlayneedle", "deletedoverlayneedle"] {
        let response = call_tool(
            &test_server(),
            "doc_navigate",
            json!({
                "query": stale_query,
                "k": 5
            }),
        )
        .await;
        let body = tool_body(response);
        let hits = body["hits"].as_array().expect("stale hits array");
        assert!(
            hits.is_empty(),
            "{stale_query} should not survive the markdown delta overlay: {hits:#?}"
        );
    }
}

#[tokio::test]
async fn doc_navigate_overlay_incomplete_base_sidecar_falls_back_to_full_write() {
    let _lock = CWD_LOCK.lock().expect("cwd lock");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let main = tempdir.path().join("main");
    std::fs::create_dir_all(&main).expect("create main worktree");
    git(&main, &["init", "-q"]);
    git(&main, &["config", "user.email", "test@spur"]);
    git(&main, &["config", "user.name", "SPUR Test"]);
    write_overlay_docs(&main);
    git(&main, &["add", "."]);
    git(&main, &["commit", "-m", "fixture"]);
    write_graph_without_sections(&main);
    let worker = create_worker_from_main(&main, "worker-incomplete");

    let _cwd = enter_dir(&worker);
    let response = call_tool(
        &test_server(),
        "doc_navigate",
        json!({
            "query": "stableoverlayneedle",
            "k": 5
        }),
    )
    .await;
    let body = tool_body(response);
    let hits = body["hits"].as_array().expect("hits array");

    assert!(
        hits.iter().any(|hit| hit["file_path"] == "docs/stable.md"),
        "incomplete base sidecar should fall back to a full overlay write: {hits:#?}"
    );
}
