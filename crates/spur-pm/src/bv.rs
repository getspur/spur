//! `BvAdapter` — wraps the `bv` (beads_viewer) CLI robot protocol for
//! graph analysis of the `.beads/` issue store.
//!
//! All methods return typed structs with a `raw: serde_json::Value` field
//! carrying the full bv output for MCP passthrough.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::DeserializeOwned;
use tokio::process::Command;

use crate::graph::*;

/// Timeout for all bv subprocess calls.
const BV_TIMEOUT: Duration = Duration::from_secs(10);

pub struct BvAdapter {
    cwd: PathBuf,
}

impl BvAdapter {
    /// Probe for `bv` binary. Returns `Err` if not installed.
    pub async fn connect(repo_root: &Path) -> anyhow::Result<Self> {
        let adapter = Self {
            cwd: repo_root.to_path_buf(),
        };

        let output = adapter.run_bv_raw(&["--version"]).await?;
        tracing::info!(version = %output.trim(), "connected to beads_viewer (bv)");

        Ok(adapter)
    }

    /// Run `bv` and return raw stdout.
    async fn run_bv_raw(&self, args: &[&str]) -> anyhow::Result<String> {
        tracing::debug!(?args, "running bv CLI");

        let child = Command::new("bv")
            .args(args)
            .current_dir(&self.cwd)
            .env("NO_COLOR", "1")
            .env("BV_OUTPUT_FORMAT", "json")
            .kill_on_drop(true)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    anyhow::anyhow!(
                        "bv binary not found. Install: brew install dicklesworthstone/tap/bv"
                    )
                } else {
                    anyhow::anyhow!("Failed to execute bv: {e}")
                }
            })?;

        let output = tokio::time::timeout(BV_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| anyhow::anyhow!("bv timed out after {}s", BV_TIMEOUT.as_secs()))?
            .map_err(|e| anyhow::anyhow!("Failed to read bv output: {e}"))?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            tracing::debug!(stderr = %stderr.trim(), "bv stderr");
        }

        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let msg = if !stderr.trim().is_empty() {
                stderr.trim().to_string()
            } else {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            };
            anyhow::bail!("bv failed: {msg}")
        }
    }

    /// Run a bv robot command, parse JSON output into a typed struct,
    /// and populate the `raw` field with the full JSON Value.
    async fn run_robot<T: DeserializeOwned + HasRaw>(
        &self,
        args: &[&str],
        cmd_name: &str,
    ) -> anyhow::Result<T> {
        let output = self.run_bv_raw(args).await?;
        let raw: serde_json::Value = serde_json::from_str(&output)
            .map_err(|e| anyhow::anyhow!("bv {cmd_name}: JSON parse error: {e}"))?;
        let mut result: T = serde_json::from_value(raw.clone())
            .map_err(|e| anyhow::anyhow!("bv {cmd_name}: deserialize error: {e}"))?;
        result.set_raw(raw);
        Ok(result)
    }

    // ─── Public graph analysis methods ────────────────────────────────

    /// Full project triage — recommendations, quick wins, blockers, health.
    pub async fn triage(&self, label: Option<&str>) -> anyhow::Result<TriageReport> {
        let mut args = vec!["--robot-triage"];
        if let Some(l) = label {
            args.push("--label");
            args.push(l);
        }
        self.run_robot(&args, "triage").await
    }

    /// Parallel execution plan with dependency-aware tracks.
    pub async fn plan(&self, label: Option<&str>) -> anyhow::Result<ExecutionPlan> {
        let mut args = vec!["--robot-plan"];
        if let Some(l) = label {
            args.push("--label");
            args.push(l);
        }
        self.run_robot(&args, "plan").await
    }

    /// Graph metrics: PageRank, betweenness, HITS, critical path, cycles.
    pub async fn insights(&self, label: Option<&str>) -> anyhow::Result<GraphInsights> {
        let mut args = vec!["--robot-insights"];
        if let Some(l) = label {
            args.push("--label");
            args.push(l);
        }
        self.run_robot(&args, "insights").await
    }

    /// Active alerts: stale issues, blocking cascades, cycles, priority mismatches.
    pub async fn alerts(&self) -> anyhow::Result<AlertReport> {
        self.run_robot(&["--robot-alerts"], "alerts").await
    }

    /// Dependency subgraph for a specific issue.
    pub async fn subgraph(
        &self,
        root_id: &str,
        depth: Option<u32>,
        format: Option<&str>,
    ) -> anyhow::Result<DependencyGraph> {
        let root_arg = format!("--graph-root={root_id}");
        let mut args = vec!["--robot-graph", &root_arg];

        let depth_str;
        if let Some(d) = depth {
            depth_str = format!("--graph-depth={d}");
            args.push(&depth_str);
        }

        let fmt_str;
        if let Some(f) = format {
            fmt_str = format!("--graph-format={f}");
            args.push(&fmt_str);
        }

        self.run_robot(&args, "graph").await
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
        let mut args = vec!["--robot-graph", "--label", label];

        let fmt_str;
        if let Some(f) = format {
            fmt_str = format!("--graph-format={f}");
            args.push(&fmt_str);
        }

        self.run_robot(&args, "graph").await
    }
}

// ─── HasRaw trait for populating the raw field ───────────────────────

/// Internal trait for setting the `raw` field on deserialized structs.
pub(crate) trait HasRaw {
    fn set_raw(&mut self, raw: serde_json::Value);
}

impl HasRaw for TriageReport {
    fn set_raw(&mut self, raw: serde_json::Value) {
        self.raw = raw;
    }
}

impl HasRaw for ExecutionPlan {
    fn set_raw(&mut self, raw: serde_json::Value) {
        self.raw = raw;
    }
}

impl HasRaw for GraphInsights {
    fn set_raw(&mut self, raw: serde_json::Value) {
        self.raw = raw;
    }
}

impl HasRaw for AlertReport {
    fn set_raw(&mut self, raw: serde_json::Value) {
        self.raw = raw;
    }
}

impl HasRaw for DependencyGraph {
    fn set_raw(&mut self, raw: serde_json::Value) {
        self.raw = raw;
    }
}
