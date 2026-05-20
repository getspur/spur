use std::path::PathBuf;

use anyhow::Context;

use spur_graph::{
    artifact_from_facts, artifact_from_facts_incremental, build_facts, load_artifact,
    resolve_worktree_root_from, write_artifact, BuildMode,
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
    let explicit_output = options.output;
    let env_output = std::env::var_os("SPUR_CODE_GRAPH_INDEX").map(PathBuf::from);
    let uses_output_override = explicit_output.is_some() || env_output.is_some();
    let output = explicit_output
        .or(env_output)
        .unwrap_or_else(|| root.join(DEFAULT_GRAPH_INDEX_PATH));

    if !options.quiet {
        println!("[spur] Building code graph index for {}", root.display());
    }

    let mut mode = BuildMode::Full;
    let (artifact, file_counts, node_count, edge_count) = if output.is_file() {
        match load_artifact(&output) {
            Ok(prev) => match artifact_from_facts_incremental(&prev, &root) {
                Ok((artifact, build_mode)) => {
                    mode = build_mode;
                    let language_counts = language_counts_from_artifact(&root, &artifact);
                    (
                        artifact.clone(),
                        language_counts,
                        artifact.symbols.len() + artifact.files.len(),
                        artifact.edges.len(),
                    )
                }
                Err(error) => {
                    tracing::info!(error = %error, "spur-graph: incremental rebuild failed; falling back to full");
                    let (facts, file_counts) = build_facts(&root)?;
                    let artifact = artifact_from_facts(&facts, &root)?;
                    let node_count = artifact.symbols.len() + artifact.files.len();
                    (artifact, file_counts, node_count, facts.edges.len())
                }
            },
            Err(error) => {
                tracing::info!(error = %error, "spur-graph: failed to load previous artifact; falling back to full");
                let (facts, file_counts) = build_facts(&root)?;
                let artifact = artifact_from_facts(&facts, &root)?;
                let node_count = artifact.symbols.len() + artifact.files.len();
                (artifact, file_counts, node_count, facts.edges.len())
            }
        }
    } else {
        let (facts, file_counts) = build_facts(&root)?;
        let artifact = artifact_from_facts(&facts, &root)?;
        let node_count = artifact.symbols.len() + artifact.files.len();
        (artifact, file_counts, node_count, facts.edges.len())
    };
    let canonical_output = if uses_output_override {
        write_artifact(&artifact, &output)?;
        None
    } else if let Some(ctx) = spur_graph::git::detect(&root) {
        spur_graph::store::cache::write_with_dedup(&artifact, &root, &ctx)?;
        spur_graph::store::cache::lookup_canonical(
            &ctx.git_common_dir,
            &artifact.manifest_version,
            &artifact.graph_content_hash,
        )
    } else {
        write_artifact(&artifact, &output)?;
        None
    };
    let language_summary = file_counts
        .iter()
        .map(|(language, count)| format!("{language}:{count}"))
        .collect::<Vec<_>>()
        .join(", ");
    let canonical_summary = canonical_output
        .as_ref()
        .map(|path| format!(", canonical: {}", path.display()))
        .unwrap_or_default();

    println!(
        "[spur] Graph index built: mode: {:?}, files: {}, nodes: {}, edges: {}, by-language: [{}], output: {}{}",
        mode,
        artifact.files.len(),
        node_count,
        edge_count,
        language_summary,
        output.display(),
        canonical_summary
    );
    Ok(())
}

fn language_counts_from_artifact(
    root: &std::path::Path,
    artifact: &spur_graph::GraphIndexArtifact,
) -> std::collections::BTreeMap<&'static str, usize> {
    let mut counts = std::collections::BTreeMap::new();
    for file in &artifact.files {
        let full = root.join(&file.file_path);
        let Some(ext) = full.extension().and_then(|s| s.to_str()) else {
            continue;
        };
        let label = match ext.to_ascii_lowercase().as_str() {
            "rs" => "rust",
            "py" => "python",
            "ts" => "typescript",
            "tsx" => "tsx",
            "md" => "markdown",
            _ => continue,
        };
        *counts.entry(label).or_insert(0) += 1;
    }
    counts
}
