use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use anyhow::Context;
use indicatif::{ProgressBar, ProgressStyle};

use spur_graph::git_walk::{run_full_walk_into, GitWalkConfig};
use spur_graph::store::commit_index::{save_artifact, save_pointer, CommitIndexPointer};
use spur_graph::store::ArtifactStagingDir;
use spur_graph::{
    artifact_from_facts, artifact_from_facts_incremental, build_facts, load_artifact,
    resolve_artifact_location, resolve_worktree_root_from, write_artifact_parquet, BuildMode,
    CommitIndexArtifact, GraphFacts, GraphIndexArtifact, TemporalShardConfig, WalkStrategy,
    WriteOptions,
};

pub const DEFAULT_GRAPH_INDEX_PATH: &str = ".spur/graph";
const COMMIT_INDEX_ARTIFACT_PATH: &str = ".spur/commit-index.json";

#[derive(Debug, Clone)]
pub struct GraphBuildOptions {
    pub root: Option<PathBuf>,
    pub workspace: bool,
    pub output: Option<PathBuf>,
    pub quiet: bool,
    pub skip_analyst: bool,
    pub with_temporal: bool,
    pub temporal_shard_config: TemporalShardConfig,
}

pub fn build(options: GraphBuildOptions) -> anyhow::Result<()> {
    let root = match (options.root, options.workspace) {
        (Some(path), _) => path,
        (None, _) => resolve_worktree_root_from(std::env::current_dir()?),
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
    if uses_output_override {
        reject_legacy_output_path(&output)?;
    }

    let temporal_shard_config = options.temporal_shard_config;
    let use_temporal = should_use_temporal(options.with_temporal);
    let warmup_stats = if !options.quiet {
        println!("[spur] Building code graph index for {}", root.display());
        let stats = WarmupStats::collect(&root, use_temporal)?;
        println!("{}", stats.line());
        Some(stats)
    } else {
        None
    };
    tracing::debug!(
        with_temporal = options.with_temporal,
        use_temporal,
        temporal_max_rows_per_shard = temporal_shard_config.max_rows_per_shard,
        temporal_max_commits_per_shard = temporal_shard_config.max_commits_per_shard,
        "spur-graph: temporal walk option evaluated"
    );

    let mut mode = BuildMode::Full;
    let previous_artifact = match resolve_artifact_location(&root, Some(&output)) {
        Ok(resolved) => {
            tracing::debug!(
                requested_path = %output.display(),
                resolved_path = %resolved.path.display(),
                format = ?resolved.format,
                "spur-graph: resolved previous artifact for graph build"
            );
            match load_previous_artifact_for_graph_build(&resolved.path) {
                Ok(prev) => Some(prev),
                Err(error) => {
                    tracing::info!(error = %error, "spur-graph: failed to load previous artifact; falling back to full");
                    None
                }
            }
        }
        Err(error) => {
            tracing::info!(error = %error, "spur-graph: no previous artifact resolved; falling back to full");
            None
        }
    };

    let (mut artifact, file_counts, node_count, edge_count) = match previous_artifact {
        Some(prev) => match artifact_from_facts_incremental(&prev, &root) {
            Ok((artifact, build_mode, _stats)) => {
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
                build_full_artifact_for_graph_build(
                    &root,
                    warmup_stats
                        .as_ref()
                        .map(|stats| extraction_progress_bar(stats.file_count)),
                )?
            }
        },
        None => build_full_artifact_for_graph_build(
            &root,
            warmup_stats
                .as_ref()
                .map(|stats| extraction_progress_bar(stats.file_count)),
        )?,
    };

    if use_temporal {
        let config = temporal_walk_config();
        let progress = warmup_stats
            .as_ref()
            .and_then(|stats| stats.temporal_commit_count().map(temporal_progress_bar));
        let result = run_full_walk_into(&root, &config, progress.clone());
        if let Some(progress) = progress {
            progress.finish_and_clear();
        }
        match result {
            Ok((temporal_artifact, commit_index)) => {
                persist_commit_index_for_graph_build(&root, &commit_index)?;
                merge_temporal_artifact(&mut artifact, temporal_artifact);
            }
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "spur-graph: temporal walk failed; writing structural-only artifact"
                );
                if !options.quiet {
                    eprintln!(
                        "[spur] Temporal graph walk failed; writing structural-only artifact: {error}"
                    );
                }
            }
        }
    }

    let canonical_output = if uses_output_override {
        let write_started = Instant::now();
        let write_span = tracing::info_span!(
            target: "spur_graph::build::write",
            "write_artifact_parquet",
            path = %output.display(),
            files = artifact.files.len(),
            symbols = artifact.symbols.len(),
            edges = artifact.edges.len()
        );
        {
            let _entered = write_span.enter();
            let result = (|| {
                let staging = ArtifactStagingDir::new(&output, &artifact.graph_content_hash)?;
                write_artifact_parquet(
                    &artifact,
                    staging.path(),
                    WriteOptions::default(),
                    Vec::new(),
                )?;
                staging.commit()
            })();
            match &result {
                Ok(_) => {
                    tracing::info!(
                        target: "spur_graph::build::write",
                        elapsed_ms = elapsed_ms(write_started),
                        "spur-graph build phase completed"
                    );
                }
                Err(error) => {
                    tracing::info!(
                        target: "spur_graph::build::write",
                        error = %error,
                        elapsed_ms = elapsed_ms(write_started),
                        "spur-graph build phase failed"
                    );
                }
            }
            result?;
        }
        None
    } else if let Some(ctx) = spur_graph::git::detect(&root) {
        let write_started = Instant::now();
        let write_span = tracing::info_span!(
            target: "spur_graph::build::write",
            "write_with_dedup",
            root = %root.display(),
            files = artifact.files.len(),
            symbols = artifact.symbols.len(),
            edges = artifact.edges.len()
        );
        {
            let _entered = write_span.enter();
            let result = spur_graph::store::cache::write_with_dedup(&artifact, &root, &ctx);
            match &result {
                Ok(()) => {
                    tracing::info!(
                        target: "spur_graph::build::write",
                        elapsed_ms = elapsed_ms(write_started),
                        "spur-graph build phase completed"
                    );
                }
                Err(error) => {
                    tracing::info!(
                        target: "spur_graph::build::write",
                        error = %error,
                        elapsed_ms = elapsed_ms(write_started),
                        "spur-graph build phase failed"
                    );
                }
            }
            result?
        }
        spur_graph::store::cache::lookup_canonical(
            &ctx.git_common_dir,
            &artifact.manifest_version,
            &artifact.graph_content_hash,
        )
    } else {
        let write_started = Instant::now();
        let write_span = tracing::info_span!(
            target: "spur_graph::build::write",
            "write_artifact_parquet",
            path = %output.display(),
            files = artifact.files.len(),
            symbols = artifact.symbols.len(),
            edges = artifact.edges.len()
        );
        {
            let _entered = write_span.enter();
            let result = (|| {
                let staging = ArtifactStagingDir::new(&output, &artifact.graph_content_hash)?;
                write_artifact_parquet(
                    &artifact,
                    staging.path(),
                    WriteOptions::default(),
                    Vec::new(),
                )?;
                staging.commit()
            })();
            match &result {
                Ok(written_dir) => {
                    if !uses_output_override {
                        if let Err(error) = spur_graph::write_current_pointer(&root, written_dir) {
                            tracing::info!(
                                target: "spur_graph::build::write",
                                error = %error,
                                elapsed_ms = elapsed_ms(write_started),
                                "spur-graph build phase failed"
                            );
                            return Err(error);
                        }
                    }
                    tracing::info!(
                        target: "spur_graph::build::write",
                        elapsed_ms = elapsed_ms(write_started),
                        "spur-graph build phase completed"
                    );
                }
                Err(error) => {
                    tracing::info!(
                        target: "spur_graph::build::write",
                        error = %error,
                        elapsed_ms = elapsed_ms(write_started),
                        "spur-graph build phase failed"
                    );
                }
            }
            result?;
        }
        None
    };
    // ---- analyst DB sync (see Task 8 / spec) ----
    if !should_skip_analyst(options.skip_analyst) {
        crate::commands::analyst::build_default(&root, options.quiet)?;
    }

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

fn should_skip_analyst(skip_analyst: bool) -> bool {
    skip_analyst || matches!(std::env::var("SPUR_GRAPH_SKIP_ANALYST"), Ok(v) if v == "1")
}

fn should_use_temporal(with_temporal: bool) -> bool {
    with_temporal || matches!(std::env::var("SPUR_GRAPH_WITH_TEMPORAL"), Ok(v) if v == "1")
}

fn temporal_walk_config() -> GitWalkConfig {
    GitWalkConfig {
        target_refs: vec!["HEAD".to_string()],
        walk_strategy: WalkStrategy::FirstParent,
        allow_replace_refs: false,
        use_gix_diff: !matches!(std::env::var("SPUR_GRAPH_USE_CLI_DIFF"), Ok(v) if v == "1"),
    }
}

#[derive(Debug, Clone, Copy)]
struct WarmupStats {
    file_count: usize,
    commit_count: WarmupCommitCount,
}

#[derive(Debug, Clone, Copy)]
enum WarmupCommitCount {
    Disabled,
    Known(usize),
    Unknown,
}

impl WarmupStats {
    fn collect(root: &Path, use_temporal: bool) -> anyhow::Result<Self> {
        let allowed_extensions = spur_graph::extract::languages::all_supported_extensions();
        let file_count = spur_graph::discovery::discover_files(root, &allowed_extensions)?.len();
        let commit_count = if use_temporal {
            count_first_parent_commits(root)
                .map(WarmupCommitCount::Known)
                .unwrap_or(WarmupCommitCount::Unknown)
        } else {
            WarmupCommitCount::Disabled
        };

        Ok(Self {
            file_count,
            commit_count,
        })
    }

    fn line(self) -> String {
        match self.commit_count {
            WarmupCommitCount::Disabled => format_warmup_stats_line(self.file_count, None),
            WarmupCommitCount::Known(commits) => {
                format_warmup_stats_line(self.file_count, Some(commits))
            }
            WarmupCommitCount::Unknown => {
                format_warmup_stats_line_with_unknown_commits(self.file_count)
            }
        }
    }

    fn temporal_commit_count(self) -> Option<usize> {
        match self.commit_count {
            WarmupCommitCount::Known(commits) => Some(commits),
            WarmupCommitCount::Disabled | WarmupCommitCount::Unknown => None,
        }
    }
}

fn count_first_parent_commits(root: &Path) -> Option<usize> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-list", "--count", "--first-parent", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn format_warmup_stats_line(file_count: usize, commit_count: Option<usize>) -> String {
    let mut line = format!("[spur]   files: {}", fmt_thousands(file_count));
    if let Some(commit_count) = commit_count {
        line.push_str(&format!(
            "   commits: {} (temporal)",
            fmt_thousands(commit_count)
        ));
    }
    line
}

fn format_warmup_stats_line_with_unknown_commits(file_count: usize) -> String {
    format!(
        "[spur]   files: {}   commits: ? (temporal)",
        fmt_thousands(file_count)
    )
}

fn fmt_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            formatted.push(',');
        }
        formatted.push(digit);
    }
    formatted
}

fn extraction_progress_bar(total: usize) -> ProgressBar {
    let progress = ProgressBar::new(u64::try_from(total).unwrap_or(u64::MAX));
    progress.set_style(
        ProgressStyle::with_template("{spinner} extracting [{bar:30}] {pos}/{len} {wide_msg}")
            .expect("valid extraction progress template"),
    );
    progress
}

fn temporal_progress_bar(total: usize) -> ProgressBar {
    let progress = ProgressBar::new(u64::try_from(total).unwrap_or(u64::MAX));
    progress.set_style(
        ProgressStyle::with_template("{spinner} temporal  [{bar:30}] {pos}/{len} commits")
            .expect("valid temporal progress template"),
    );
    progress
}

fn merge_temporal_artifact(artifact: &mut GraphIndexArtifact, temporal: GraphIndexArtifact) {
    artifact.commits = temporal.commits;
    artifact.symbol_snapshots = temporal.symbol_snapshots;
    artifact.temporal_edges = temporal.temporal_edges;
    artifact.diagnostics.extend(temporal.diagnostics);
}

fn persist_commit_index_for_graph_build(
    root: &Path,
    commit_index: &CommitIndexArtifact,
) -> anyhow::Result<()> {
    save_artifact(root, COMMIT_INDEX_ARTIFACT_PATH, commit_index)
        .with_context(|| format!("write commit-index artifact at {COMMIT_INDEX_ARTIFACT_PATH}"))?;
    save_pointer(
        root,
        &CommitIndexPointer {
            schema_version: commit_index.schema_version,
            artifact_relative_path: COMMIT_INDEX_ARTIFACT_PATH.to_string(),
            indexed_at: commit_index.indexed_at.clone(),
            refs: commit_index.refs.clone(),
        },
    )
    .context("write commit-index pointer")
}

fn reject_legacy_output_path(output: &Path) -> anyhow::Result<()> {
    if output.extension().and_then(|ext| ext.to_str()) == Some("json") {
        anyhow::bail!(
            "graph build now writes a Parquet directory layout; `{}` looks like a legacy JSON file path. Pass --output with a directory path such as `.spur/graph`.",
            output.display()
        );
    }
    if output.is_file() {
        anyhow::bail!(
            "graph build now writes a Parquet directory layout; `{}` is an existing file. Pass --output with a directory path.",
            output.display()
        );
    }
    Ok(())
}

fn load_previous_artifact_for_graph_build(path: &Path) -> anyhow::Result<GraphIndexArtifact> {
    let load_started = Instant::now();
    let load_span = tracing::info_span!(
        target: "spur_graph::build::load",
        "load_artifact",
        path = %path.display(),
    );
    let load_result = {
        let _entered = load_span.enter();
        let result = load_artifact(path);
        match &result {
            Ok(prev) => {
                tracing::info!(
                    target: "spur_graph::build::load",
                    files = prev.file_manifests.len(),
                    symbols = prev.symbols.len(),
                    edges = prev.edges.len(),
                    elapsed_ms = elapsed_ms(load_started),
                    "spur-graph build phase completed"
                );
            }
            Err(error) => {
                tracing::info!(
                    target: "spur_graph::build::load",
                    error = %error,
                    elapsed_ms = elapsed_ms(load_started),
                    "spur-graph build phase failed"
                );
            }
        }
        result
    };
    load_result
}

fn build_full_artifact_for_graph_build(
    root: &Path,
    progress: Option<ProgressBar>,
) -> anyhow::Result<(
    GraphIndexArtifact,
    BTreeMap<&'static str, usize>,
    usize,
    usize,
)> {
    let facts_result = build_facts_for_graph_build(root, progress.clone());
    if let Some(progress) = progress {
        progress.finish_and_clear();
    }
    let (facts, file_counts) = facts_result?;
    let artifact = artifact_from_facts_for_graph_build(&facts, root)?;
    let node_count = artifact.symbols.len() + artifact.files.len();
    Ok((artifact, file_counts, node_count, facts.edges.len()))
}

fn build_facts_for_graph_build(
    root: &Path,
    progress: Option<ProgressBar>,
) -> anyhow::Result<(GraphFacts, BTreeMap<&'static str, usize>)> {
    let extract_started = Instant::now();
    let extract_span = tracing::info_span!(
        target: "spur_graph::build::extract_full",
        "build_facts",
        root = %root.display()
    );
    let result = {
        let _entered = extract_span.enter();
        let result = build_facts(root, progress);
        match &result {
            Ok((facts, file_counts)) => {
                tracing::info!(
                    target: "spur_graph::build::extract_full",
                    files = file_counts.values().sum::<usize>(),
                    nodes = facts.nodes.len(),
                    edges = facts.edges.len(),
                    elapsed_ms = elapsed_ms(extract_started),
                    "spur-graph build phase completed"
                );
            }
            Err(error) => {
                tracing::info!(
                    target: "spur_graph::build::extract_full",
                    error = %error,
                    elapsed_ms = elapsed_ms(extract_started),
                    "spur-graph build phase failed"
                );
            }
        }
        result
    };
    result
}

fn artifact_from_facts_for_graph_build(
    facts: &GraphFacts,
    root: &Path,
) -> anyhow::Result<GraphIndexArtifact> {
    let artifact_started = Instant::now();
    let artifact_span = tracing::info_span!(
        target: "spur_graph::build::artifact",
        "artifact_from_facts",
        root = %root.display()
    );
    let result = {
        let _entered = artifact_span.enter();
        let result = artifact_from_facts(facts, root);
        match &result {
            Ok(artifact) => {
                tracing::info!(
                    target: "spur_graph::build::artifact",
                    files = artifact.files.len(),
                    symbols = artifact.symbols.len(),
                    edges = artifact.edges.len(),
                    elapsed_ms = elapsed_ms(artifact_started),
                    "spur-graph build phase completed"
                );
            }
            Err(error) => {
                tracing::info!(
                    target: "spur_graph::build::artifact",
                    error = %error,
                    elapsed_ms = elapsed_ms(artifact_started),
                    "spur-graph build phase failed"
                );
            }
        }
        result
    };
    result
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
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

#[cfg(test)]
mod tests {
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

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            std::env::remove_var(key);
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

    #[test]
    fn should_skip_analyst_honors_option_and_env_flags() {
        {
            let _env = EnvGuard::remove("SPUR_GRAPH_SKIP_ANALYST");
            assert!(super::should_skip_analyst(true));
        }
        {
            let _env = EnvGuard::set("SPUR_GRAPH_SKIP_ANALYST", "1");
            assert!(super::should_skip_analyst(false));
        }
        {
            let _env = EnvGuard::set("SPUR_GRAPH_SKIP_ANALYST", "true");
            assert!(!super::should_skip_analyst(false));
        }
    }

    #[test]
    fn should_use_temporal_honors_option_and_env_flags() {
        {
            let _env = EnvGuard::remove("SPUR_GRAPH_WITH_TEMPORAL");
            assert!(super::should_use_temporal(true));
        }
        {
            let _env = EnvGuard::set("SPUR_GRAPH_WITH_TEMPORAL", "1");
            assert!(super::should_use_temporal(false));
        }
        {
            let _env = EnvGuard::set("SPUR_GRAPH_WITH_TEMPORAL", "true");
            assert!(!super::should_use_temporal(false));
        }
    }

    #[test]
    fn fmt_thousands_inserts_commas() {
        assert_eq!(super::fmt_thousands(0), "0");
        assert_eq!(super::fmt_thousands(12), "12");
        assert_eq!(super::fmt_thousands(1_234), "1,234");
        assert_eq!(super::fmt_thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn format_warmup_stats_line_includes_temporal_commit_count_when_present() {
        assert_eq!(
            super::format_warmup_stats_line(1_234, Some(5_678)),
            "[spur]   files: 1,234   commits: 5,678 (temporal)"
        );
    }

    #[test]
    fn format_warmup_stats_line_omits_commit_count_without_temporal() {
        assert_eq!(
            super::format_warmup_stats_line(1_234, None),
            "[spur]   files: 1,234"
        );
    }

    #[test]
    fn format_warmup_stats_line_marks_unknown_temporal_commits() {
        assert_eq!(
            super::format_warmup_stats_line_with_unknown_commits(1_234),
            "[spur]   files: 1,234   commits: ? (temporal)"
        );
    }
}
