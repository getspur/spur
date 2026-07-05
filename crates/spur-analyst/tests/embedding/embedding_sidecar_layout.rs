use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate lives under crates/spur-analyst")
        .to_path_buf()
}

fn read_repo_file(path: &str) -> String {
    fs::read_to_string(repo_root().join(path)).unwrap_or_else(|error| {
        panic!("failed to read {path}: {error}");
    })
}

fn line_count(source: &str) -> usize {
    source.lines().count()
}

#[test]
fn sidecar_embedding_modules_live_under_embedding_namespace() {
    let root = repo_root();
    let expected_paths = [
        "crates/spur-analyst/src/embedding/sidecar_client.rs",
        "crates/spur-analyst/src/embedding/sidecar_service.rs",
        "crates/spur-analyst/src/embedding/protocol.rs",
    ];
    for path in expected_paths {
        assert!(root.join(path).exists(), "{path} should exist");
    }

    let retired_paths = [
        "crates/spur-analyst/src/embed_client.rs",
        "crates/spur-analyst/src/embed_service.rs",
    ];
    for path in retired_paths {
        assert!(!root.join(path).exists(), "{path} should be deleted");
    }
}

#[test]
fn sidecar_modules_stay_within_refactor_budgets() {
    let budgets = [
        ("crates/spur-analyst/src/embedding/sidecar_client.rs", 180),
        ("crates/spur-analyst/src/embedding/sidecar_service.rs", 300),
        ("crates/spur-analyst/src/embedding/protocol.rs", 120),
    ];

    for (path, budget) in budgets {
        let source = read_repo_file(path);
        assert!(
            line_count(&source) < budget,
            "{path} should stay below {budget} lines"
        );
    }
}

#[test]
fn sidecar_service_uses_embedding_runtime_boundary() {
    let source = read_repo_file("crates/spur-analyst/src/embedding/sidecar_service.rs");

    assert!(
        source.contains("EmbeddingRuntime"),
        "sidecar service should use EmbeddingRuntime"
    );
    assert!(
        !source.contains("crate::mcp") && !source.contains("spur_mcp"),
        "sidecar service must not import through MCP"
    );
    assert!(
        !source.contains("embed_model_cell")
            && !source.contains("load_embed_model")
            && !source.contains("fastembed::TextEmbedding"),
        "sidecar service should not reach into model-cache internals"
    );
}

#[test]
fn cli_imports_sidecar_service_from_embedding_namespace() {
    let source = read_repo_file("crates/spur-cli/src/commands/embed.rs");

    assert!(
        source.contains("spur_analyst::embedding::sidecar_service::serve"),
        "CLI should call the embedding sidecar service namespace"
    );
    assert!(
        !source.contains("spur_analyst::embed_service"),
        "CLI should not call the retired top-level embed_service module"
    );
}
