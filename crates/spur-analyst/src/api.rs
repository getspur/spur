use std::{env, path::Path};

use anyhow::{anyhow, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AnalystDuckDbResourceCaps {
    pub(crate) memory_limit: String,
    pub(crate) threads: usize,
}

impl Default for AnalystDuckDbResourceCaps {
    fn default() -> Self {
        Self {
            memory_limit: crate::DEFAULT_ANALYST_DUCKDB_MEMORY_LIMIT.to_owned(),
            threads: crate::DEFAULT_ANALYST_DUCKDB_THREADS,
        }
    }
}

impl AnalystDuckDbResourceCaps {
    pub(crate) fn from_env() -> Self {
        let mut caps = Self::default();
        match env::var(crate::ANALYST_DUCKDB_MEMORY_LIMIT_ENV) {
            Ok(value) => {
                let value = value.trim();
                if value.is_empty() {
                    tracing::warn!(
                        env_var = crate::ANALYST_DUCKDB_MEMORY_LIMIT_ENV,
                        default = crate::DEFAULT_ANALYST_DUCKDB_MEMORY_LIMIT,
                        "invalid empty analyst DuckDB memory limit override; using default"
                    );
                } else {
                    caps.memory_limit = value.to_owned();
                }
            }
            Err(env::VarError::NotPresent) => {}
            Err(error) => tracing::warn!(
                env_var = crate::ANALYST_DUCKDB_MEMORY_LIMIT_ENV,
                error = %error,
                default = crate::DEFAULT_ANALYST_DUCKDB_MEMORY_LIMIT,
                "invalid analyst DuckDB memory limit override; using default"
            ),
        }

        match env::var(crate::ANALYST_DUCKDB_THREADS_ENV) {
            Ok(value) => match value.trim().parse::<usize>() {
                Ok(threads) if threads > 0 => caps.threads = threads,
                _ => tracing::warn!(
                    env_var = crate::ANALYST_DUCKDB_THREADS_ENV,
                    value = %value,
                    default = crate::DEFAULT_ANALYST_DUCKDB_THREADS,
                    "invalid analyst DuckDB threads override; using default"
                ),
            },
            Err(env::VarError::NotPresent) => {}
            Err(error) => tracing::warn!(
                env_var = crate::ANALYST_DUCKDB_THREADS_ENV,
                error = %error,
                default = crate::DEFAULT_ANALYST_DUCKDB_THREADS,
                "invalid analyst DuckDB threads override; using default"
            ),
        }

        caps
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnowledgeSearchScope {
    All,
    Docs,
    Code,
    Graph,
}

impl KnowledgeSearchScope {
    pub(crate) fn as_sql_scope(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Docs => "docs",
            Self::Code => "code",
            Self::Graph => "graph",
        }
    }
}

impl TryFrom<&str> for KnowledgeSearchScope {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self> {
        match value {
            "all" => Ok(Self::All),
            "docs" => Ok(Self::Docs),
            "code" => Ok(Self::Code),
            "graph" => Ok(Self::Graph),
            other => Err(anyhow!(
                "knowledge search scope must be one of all|docs|code|graph, got {other:?}"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnowledgeQueryIntent {
    Explain,
    Change,
    Review,
    Debug,
    Plan,
}

impl KnowledgeQueryIntent {
    pub(crate) fn as_sql_intent(self) -> &'static str {
        match self {
            Self::Explain => "explain",
            Self::Change => "change",
            Self::Review => "review",
            Self::Debug => "debug",
            Self::Plan => "plan",
        }
    }
}

#[derive(Debug, Clone)]
pub struct KnowledgeQueryOptions {
    pub limit: usize,
    pub intent: KnowledgeQueryIntent,
    pub query_vec: Option<Vec<f32>>,
}

impl Default for KnowledgeQueryOptions {
    fn default() -> Self {
        Self {
            limit: 20,
            intent: KnowledgeQueryIntent::Explain,
            query_vec: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeCandidate {
    pub kind: String,
    pub title: String,
    pub file_path: String,
    pub stable_symbol_id: Option<String>,
    pub symbol_kind: Option<String>,
    pub score: f64,
    pub signal: Option<String>,
    pub neighbor_kind: Option<String>,
    pub edge_bind_method: Option<String>,
    pub grounding: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KnowledgeQueryResult {
    pub db_path: String,
    pub graph_content_hash: Option<String>,
    pub candidates: Vec<KnowledgeCandidate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolEvidenceStatus {
    Available,
    MissingSymbol,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SymbolEvidenceCaveat {
    pub stable_symbol_id: Option<String>,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SymbolRiskScorecardRow {
    pub input_index: usize,
    pub stable_symbol_id: String,
    pub status: SymbolEvidenceStatus,
    pub entity_name: Option<String>,
    pub qualified_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub file_path: Option<String>,
    pub pagerank: Option<f64>,
    pub in_degree: Option<i64>,
    pub out_degree: Option<i64>,
    pub callers: Option<i64>,
    pub importers: Option<i64>,
    pub inbound_total: Option<i64>,
    pub churn_90d: Option<i64>,
    pub last_touched: Option<String>,
    pub blast_radius_score: Option<f64>,
    pub posture: Option<String>,
    pub caveats: Vec<SymbolEvidenceCaveat>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SymbolCommunityContextRow {
    pub input_index: usize,
    pub stable_symbol_id: String,
    pub status: SymbolEvidenceStatus,
    pub component_id: Option<i64>,
    pub component_size: Option<i64>,
    pub community_id: Option<i64>,
    pub caveats: Vec<SymbolEvidenceCaveat>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SymbolGraphMetrics {
    pub calls_edges: Option<i64>,
    pub connected_nodes: Option<i64>,
    pub components: Option<i64>,
    pub largest_component: Option<i64>,
    pub communities: Option<i64>,
    pub density: Option<f64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SymbolRiskCommunityResult {
    pub db_path: String,
    pub graph_content_hash: Option<String>,
    pub max_symbols: usize,
    pub truncated: bool,
    pub risk_scorecard: Vec<SymbolRiskScorecardRow>,
    pub community_context: Vec<SymbolCommunityContextRow>,
    pub graph_metrics: Option<SymbolGraphMetrics>,
    pub caveats: Vec<SymbolEvidenceCaveat>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgePathEngine {
    DuckPgq,
    RecursiveSql,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgePathStatus {
    PathFound,
    NoPath,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnowledgePathOptions {
    pub max_hops: usize,
    pub max_paths: usize,
    pub undirected: bool,
}

impl Default for KnowledgePathOptions {
    fn default() -> Self {
        Self {
            max_hops: 4,
            max_paths: 6,
            undirected: false,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KnowledgePathRow {
    pub path_index: usize,
    pub hop_index: usize,
    pub source_stable_id: String,
    pub target_stable_id: String,
    pub relation: Option<String>,
    pub edge_kind: Option<String>,
    pub confidence: Option<String>,
    pub bind_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    pub engine: KnowledgePathEngine,
    pub status: KnowledgePathStatus,
    pub caveat: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KnowledgePathResult {
    pub db_path: String,
    pub graph_content_hash: Option<String>,
    pub max_hops: usize,
    pub max_paths: usize,
    pub engine: KnowledgePathEngine,
    pub status: KnowledgePathStatus,
    pub caveat: Option<String>,
    pub rows: Vec<KnowledgePathRow>,
}

pub(crate) struct KnowledgePathResultContext<'a> {
    pub(crate) db_path: &'a Path,
    pub(crate) graph_content_hash: Option<String>,
    pub(crate) max_hops: usize,
    pub(crate) max_paths: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct SymbolInput {
    pub(crate) input_index: usize,
    pub(crate) stable_symbol_id: String,
}
