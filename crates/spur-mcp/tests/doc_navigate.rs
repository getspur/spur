use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use spur_acp::{BrainSessionId, SessionId};
use spur_graph::store::lance_sections::write_sections_dataset;
use spur_graph::{
    artifact_from_facts, build_facts, write_artifact_parquet, write_current_pointer, WriteOptions,
};
use spur_mcp::server::{community_feature_gate, DetachedContinuationCtx};
use spur_mcp::McpCallbackServer;

static CWD_LOCK: Mutex<()> = Mutex::new(());

struct CwdGuard {
    original: PathBuf,
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.original);
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
