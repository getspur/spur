//! `BvAdapter` — wraps the native `GraphEngine` and provides the historical
//! API surface that MCP, orchestrator, TUI, and tests already call.
//!
//! All methods return typed structs with a `raw: serde_json::Value` field
//! containing the report's JSON for MCP passthrough.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::beads_crate::{AdapterConfig, BeadsCrateAdapter};
use crate::graph::{AlertReport, DependencyGraph, ExecutionPlan, GraphInsights, TriageReport};
use crate::graph_engine::{GraphEngine, GraphEngineConfig};

pub struct BvAdapter {
    engine: GraphEngine,
}

impl BvAdapter {
    /// Construct a BvAdapter from a connected BeadsCrateAdapter and graph config.
    pub fn from_beads(beads: Arc<BeadsCrateAdapter>, cfg: GraphEngineConfig) -> Self {
        Self {
            engine: GraphEngine::new(beads, cfg),
        }
    }

    /// Compatibility constructor matching the historical signature.
    pub async fn connect(repo_root: &Path) -> anyhow::Result<Self> {
        let beads = Arc::new(
            BeadsCrateAdapter::open(&beads_dir_for(repo_root), AdapterConfig::default()).await?,
        );
        Ok(Self::from_beads(beads, GraphEngineConfig::default()))
    }

    // ─── Public graph analysis methods ────────────────────────────────

    /// Full project triage - recommendations, quick wins, blockers, health.
    pub async fn triage(&self, label: Option<&str>) -> anyhow::Result<TriageReport> {
        self.engine.triage(label).await
    }

    /// Parallel execution plan with dependency-aware tracks.
    pub async fn plan(&self, label: Option<&str>) -> anyhow::Result<ExecutionPlan> {
        self.engine.plan(label).await
    }

    /// Graph metrics: PageRank, betweenness, HITS, critical path, cycles.
    pub async fn insights(&self, label: Option<&str>) -> anyhow::Result<GraphInsights> {
        self.engine.insights(label).await
    }

    /// Active alerts: stale issues, blocking cascades, cycles, priority mismatches.
    pub async fn alerts(&self) -> anyhow::Result<AlertReport> {
        self.engine.alerts().await
    }

    /// Dependency subgraph for a specific issue.
    pub async fn subgraph(
        &self,
        root_id: &str,
        depth: Option<u32>,
        format: Option<&str>,
    ) -> anyhow::Result<DependencyGraph> {
        self.engine.subgraph(root_id, depth, format).await
    }

    /// Dependency graph for all issues matching a label.
    ///
    /// This is the correct projection for a plan-backed epic: the epic issue
    /// is the durable anchor, while `spur:plan-id:<id>` is the scope that
    /// includes all child issues and their DAG edges.
    pub async fn graph_by_label(
        &self,
        label: &str,
        format: Option<&str>,
    ) -> anyhow::Result<DependencyGraph> {
        self.engine.graph_by_label(label, format).await
    }
}

fn beads_dir_for(repo_root: &Path) -> PathBuf {
    if repo_root.join("beads.db").is_file() {
        repo_root.to_path_buf()
    } else {
        repo_root.join(".beads")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::beads_crate::{AdapterConfig, BeadsCrateAdapter};
    use crate::graph_engine::{GraphEngine, GraphEngineConfig};
    use crate::test_workspace::TestBeadsWorkspace;

    use super::*;

    async fn open_beads(w: &TestBeadsWorkspace) -> Arc<BeadsCrateAdapter> {
        Arc::new(
            BeadsCrateAdapter::open(w.path(), AdapterConfig::default())
                .await
                .expect("open beads crate adapter"),
        )
    }

    #[tokio::test(flavor = "current_thread")]
    async fn from_beads_delegates_triage_to_graph_engine() {
        let mut w = TestBeadsWorkspace::init();
        let label = "spur:plan-id:T16";
        let issue = w.create_issue("Adapter task");
        w.add_label(&issue, label);

        let beads = open_beads(&w).await;
        let cfg = GraphEngineConfig::default();
        let expected = GraphEngine::new(Arc::clone(&beads), cfg.clone())
            .triage(Some(label))
            .await
            .expect("graph engine triage");
        let adapter = BvAdapter::from_beads(beads, cfg);

        let actual = adapter.triage(Some(label)).await.expect("adapter triage");
        assert_eq!(actual.triage.quick_ref.open_count, 1);
        assert_eq!(actual.triage.recommendations[0].id, issue);
        assert_eq!(actual.data_hash, expected.data_hash);
        assert_eq!(actual.raw["triage"], expected.raw["triage"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn connect_keeps_repo_root_signature_and_uses_local_beads() {
        let repo = tempfile::TempDir::new().expect("create repo tempdir");
        let beads_dir = repo.path().join(".beads");
        std::fs::create_dir_all(&beads_dir).expect("create .beads dir");

        let mut w = TestBeadsWorkspace::init();
        let issue = w.create_issue("Repo root adapter task");
        w.copy_db_to(&beads_dir);

        let adapter = BvAdapter::connect(repo.path())
            .await
            .expect("connect from repo root");
        let report = adapter.triage(None).await.expect("triage from repo root");

        assert_eq!(report.triage.quick_ref.open_count, 1);
        assert_eq!(report.triage.recommendations[0].id, issue);
    }

    #[test]
    fn source_no_longer_contains_bv_subprocess_path() {
        let source = include_str!("bv.rs");
        let command_new_bv = ["Command", "::new", "(\"", "bv", "\")"].concat();
        let tokio_command = ["tokio", "::process", "::Command"].concat();

        assert!(!source.contains(&command_new_bv));
        assert!(!source.contains(&tokio_command));
    }
}
