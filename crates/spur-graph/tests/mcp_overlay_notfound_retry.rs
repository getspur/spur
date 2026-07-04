//! Regression coverage for a control-flow gap in
//! `code_graph_backend_response_with_refresh`: the dirty-worktree overlay/rebuild
//! retry only ran after a *successful* handler response revealed stale file
//! OIDs. Any code_* handler whose "not found" path is an `Err` (code_resolve,
//! code_read_symbol, code_symbol_info, ...) short-circuited past that retry via
//! `?`, so a brand-new symbol that exists only in an uncommitted worktree edit
//! was reported as missing even though `code_symbol_search` (whose 0-match
//! path is a successful empty list, not an error) found it fine.

mod support;

use serde_json::json;
use spur_graph::mcp::{with_worktree_root_for_request, CodeGraphResult, GraphMcpModule};
use spur_graph::{
    artifact_from_facts, build_facts, write_artifact_parquet, write_current_pointer,
    GraphIndexArtifact, WriteOptions,
};
use support::git_repo::GitRepo;

fn build_full(root: &std::path::Path) -> GraphIndexArtifact {
    let (facts, _counts) = build_facts(root, None).expect("build facts");
    artifact_from_facts(&facts, root).expect("build artifact")
}

fn write_current_only_cache(root: &std::path::Path, artifact: &GraphIndexArtifact) {
    let artifact_base = root.join(".spur/graph");
    let written = write_artifact_parquet(
        artifact,
        &artifact_base,
        WriteOptions::default(),
        Vec::new(),
    )
    .expect("write worktree graph artifact");
    write_current_pointer(root, &written).expect("write CURRENT pointer");
}

async fn dispatch_in_repo(
    module: &GraphMcpModule,
    repo_root: &std::path::Path,
    name: &str,
    args: serde_json::Value,
) -> CodeGraphResult {
    with_worktree_root_for_request(repo_root.to_path_buf(), module.dispatch(name, args)).await
}

/// Sets up a committed baseline artifact, then dirties the worktree with a
/// brand-new untracked symbol that the indexed artifact has never seen.
fn dirty_worktree_with_new_symbol() -> GitRepo {
    let repo = GitRepo::new();
    repo.write("src/lib.rs", "pub fn existing_symbol() {}\n");
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-m", "baseline"]);

    let artifact = build_full(repo.path());
    write_current_only_cache(repo.path(), &artifact);

    // Dirty (untracked, uncommitted): the indexed artifact above has no idea
    // this symbol exists.
    repo.write(
        "src/new_module.rs",
        "pub fn brand_new_dirty_symbol() -> u64 { 7 }\n",
    );
    repo
}

#[tokio::test]
async fn code_symbol_search_already_finds_dirty_worktree_symbol_via_overlay() {
    let repo = dirty_worktree_with_new_symbol();
    let module = GraphMcpModule::default();

    let search = dispatch_in_repo(
        &module,
        repo.path(),
        "code_symbol_search",
        json!({"query": "brand_new_dirty_symbol", "mode": "exact"}),
    )
    .await
    .expect("code_symbol_search should succeed even when the base artifact has 0 matches");

    assert_eq!(
        search["total_matches"].as_u64(),
        Some(1),
        "code_symbol_search should find the dirty-worktree symbol via its overlay retry: {search:#?}"
    );
}

#[tokio::test]
async fn code_resolve_finds_dirty_worktree_symbol_via_overlay() {
    let repo = dirty_worktree_with_new_symbol();
    let module = GraphMcpModule::default();

    let resolve = dispatch_in_repo(
        &module,
        repo.path(),
        "code_resolve",
        json!({"selector": "brand_new_dirty_symbol"}),
    )
    .await;

    assert!(
        resolve.is_ok(),
        "code_resolve should retry against the dirty-worktree overlay before reporting \
         not-found, exactly like code_symbol_search already does; got: {resolve:?}"
    );
}

#[tokio::test]
async fn code_read_symbol_finds_dirty_worktree_symbol_via_overlay() {
    let repo = dirty_worktree_with_new_symbol();
    let module = GraphMcpModule::default();

    let read = dispatch_in_repo(
        &module,
        repo.path(),
        "code_read_symbol",
        json!({
            "path": "src/new_module.rs",
            "name": "brand_new_dirty_symbol",
            "response_format": "source",
        }),
    )
    .await;

    assert!(
        read.is_ok(),
        "code_read_symbol should retry against the dirty-worktree overlay before reporting \
         'missing from graph artifact', exactly like code_symbol_search already does; got: {read:?}"
    );
}

#[tokio::test]
async fn code_resolve_still_fails_fast_for_a_genuinely_missing_symbol_in_a_clean_worktree() {
    let repo = GitRepo::new();
    repo.write("src/lib.rs", "pub fn existing_symbol() {}\n");
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-m", "baseline"]);

    let artifact = build_full(repo.path());
    write_current_only_cache(repo.path(), &artifact);
    // Worktree stays clean: no dirty files at all.

    let module = GraphMcpModule::default();
    let resolve = dispatch_in_repo(
        &module,
        repo.path(),
        "code_resolve",
        json!({"selector": "this_symbol_truly_does_not_exist_anywhere"}),
    )
    .await;

    assert!(
        resolve.is_err(),
        "a genuinely nonexistent symbol in a clean worktree should still fail, not silently succeed"
    );
}
