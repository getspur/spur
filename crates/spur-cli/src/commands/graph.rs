use std::path::PathBuf;

use anyhow::Context;

use spur_graph::{
    artifact_from_facts, extract_rust_worktree, resolve_worktree_root_from, write_artifact,
};

pub const DEFAULT_GRAPH_INDEX_PATH: &str = ".spur/graph-index.json";

#[derive(Debug, Clone)]
pub struct GraphBuildOptions {
    pub root: Option<PathBuf>,
    pub output: Option<PathBuf>,
    pub quiet: bool,
}

pub fn build(options: GraphBuildOptions) -> anyhow::Result<()> {
    let root = match options.root {
        Some(path) => path,
        None => resolve_worktree_root_from(std::env::current_dir()?),
    };
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to canonicalize root `{}`", root.display()))?;
    let output = options
        .output
        .or_else(|| std::env::var_os("SPUR_CODE_GRAPH_INDEX").map(PathBuf::from))
        .unwrap_or_else(|| root.join(DEFAULT_GRAPH_INDEX_PATH));

    if !options.quiet {
        println!("[spur] Building code graph index for {}", root.display());
    }

    let facts = extract_rust_worktree(&root)?;
    let artifact = artifact_from_facts(&facts, &root)?;
    write_artifact(&artifact, &output)?;

    println!(
        "[spur] Graph index built: files: {}, nodes: {}, edges: {}, output: {}",
        artifact.files.len(),
        facts.nodes.len(),
        facts.edges.len(),
        output.display()
    );
    Ok(())
}
