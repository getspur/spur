#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use jute::{
        backend::notebook::{
            Cell, CellDagMetadata, CellMetadata, CodeCell, DagSource, FrontendCellMetadata,
            MultilineString, NotebookMetadata, NotebookRoot, Output, OutputError, PortSpec,
            SpurCellMetadata,
        },
        commands::{Column, DatasourceEntry, DatasourceKind},
    };
    use serde_json::Map;

    use crate::context::refs::Ref;

    use super::*;

    fn notebook(cells: Vec<Cell>) -> NotebookRoot {
        NotebookRoot {
            metadata: NotebookMetadata {
                kernelspec: None,
                language_info: None,
                orig_nbformat: None,
                title: None,
                authors: None,
                jute_deck: None,
                other: Default::default(),
            },
            nbformat_minor: 5,
            nbformat: 4,
            cells,
        }
    }

    fn port(name: &str) -> PortSpec {
        PortSpec {
            port: name.to_string(),
            repr: "arrow".to_string(),
            display: None,
            class: None,
            schema: None,
        }
    }

    fn source(kind: &str, port: &str) -> DagSource {
        DagSource {
            kind: kind.to_string(),
            port: port.to_string(),
            class: None,
            schema: None,
        }
    }

    fn code_cell(
        id: &str,
        version: u64,
        produces: Vec<&str>,
        consumes: Vec<&str>,
        dag_source: Option<DagSource>,
        frontend: Option<FrontendCellMetadata>,
        outputs: Vec<Output>,
    ) -> Cell {
        Cell::Code(CodeCell {
            id: Some(id.to_string()),
            metadata: CellMetadata {
                spur: Some(SpurCellMetadata {
                    version,
                    last_edited_by: None,
                    datasource_setup: None,
                    dag: Some(CellDagMetadata {
                        produces: produces.into_iter().map(port).collect(),
                        consumes: consumes.into_iter().map(str::to_string).collect(),
                        source: dag_source,
                    }),
                    code_type: None,
                    frontend,
                    cron: None,
                }),
                jute_deck: None,
                other: Default::default(),
            },
            source: MultilineString::Single("print('ok')".to_string()),
            execution_count: Some(3),
            outputs,
        })
    }

    fn entries() -> Vec<DatasourceEntry> {
        vec![DatasourceEntry {
            name: "sales".to_string(),
            path: "/data/sales.csv".to_string(),
            kind: DatasourceKind::Csv,
            group: None,
            columns: vec![Column {
                name: "amount".to_string(),
                sql_type: "DOUBLE".to_string(),
            }],
            row_count: Some(10),
            tables: Vec::new(),
        }]
    }

    fn port_versions() -> BTreeMap<String, u64> {
        BTreeMap::from([("raw".to_string(), 4), ("view".to_string(), 2)])
    }

    fn fixture_root() -> NotebookRoot {
        notebook(vec![
            code_cell(
                "src",
                7,
                vec!["raw"],
                Vec::new(),
                Some(source("csv", "sales")),
                None,
                Vec::new(),
            ),
            code_cell(
                "viz",
                8,
                Vec::new(),
                vec!["raw"],
                None,
                Some(FrontendCellMetadata {
                    kind: Some("html".to_string()),
                    binds: vec!["raw".to_string()],
                    emits: vec!["view".to_string()],
                }),
                Vec::new(),
            ),
        ])
    }

    fn cyclic_metadata_fixture() -> NotebookRoot {
        notebook(vec![
            code_cell("a", 1, vec!["a-out"], vec!["b-out"], None, None, Vec::new()),
            code_cell("b", 1, vec!["b-out"], vec!["a-out"], None, None, Vec::new()),
        ])
    }

    fn fixture_with_error_output() -> NotebookRoot {
        notebook(vec![code_cell(
            "bad",
            9,
            Vec::new(),
            Vec::new(),
            None,
            None,
            vec![Output::Error(OutputError {
                ename: "KeyError".to_string(),
                evalue: "'volume'".to_string(),
                traceback: Vec::new(),
                other: Map::new(),
            })],
        )])
    }

    #[test]
    fn upstream_walk_from_port_reaches_source_datasource() {
        let graph = LineageGraph::build(&fixture_root(), &entries(), &port_versions());
        let out = graph.walk(&Ref::parse("port://raw").unwrap(), Direction::Upstream, 3);

        assert!(out.nodes.iter().any(|node| node.r#ref.starts_with("ds://")));
        assert!(out.edges.iter().all(|edge| edge.provenance == "declared"));
    }

    #[test]
    fn depth_bound_and_cycle_cap_hold() {
        let graph = LineageGraph::build(&cyclic_metadata_fixture(), &[], &BTreeMap::new());
        let out = graph.walk(&Ref::parse("cell://a").unwrap(), Direction::Both, 50);

        assert!(out.nodes.len() <= 100);
        assert!(out.truncated);
    }

    #[test]
    fn failed_cell_carries_error_excerpt() {
        let graph = LineageGraph::build(&fixture_with_error_output(), &[], &BTreeMap::new());
        let out = graph.walk(&Ref::parse("cell://bad").unwrap(), Direction::Both, 1);
        let job = out
            .nodes
            .iter()
            .find(|node| node.r#ref.starts_with("cell://bad"))
            .unwrap();

        assert_eq!(job.state, "failed");
        assert!(job.error_excerpt.as_deref().unwrap().contains("KeyError"));
    }
}
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use jute::{
    backend::notebook::{Cell, CellDagMetadata, FrontendCellMetadata, NotebookRoot, Output},
    commands::{DatasourceEntry, DatasourceKind},
};
use serde::Serialize;

use crate::context::{catalog::datasource_id, refs::Ref};

pub const DEFAULT_WALK_DEPTH: usize = 3;
const VISITED_NODE_CAP: usize = 100;
const ERROR_EXCERPT_CAP: usize = 160;
const ROLE_DATASET: &str = "dataset";
const ROLE_JOB: &str = "job";
const STATE_FRESH: &str = "fresh";
const STATE_FAILED: &str = "failed";
const STATE_UNKNOWN: &str = "unknown";
const PROVENANCE_DECLARED: &str = "declared";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Upstream,
    Downstream,
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineageNode {
    pub r#ref: String,
    pub role: &'static str,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_excerpt: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineageEdge {
    pub from: String,
    pub to: String,
    pub via: &'static str,
    pub provenance: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineageWalk {
    pub root: String,
    pub nodes: Vec<LineageNode>,
    pub edges: Vec<LineageEdge>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LineageGraph {
    nodes: BTreeMap<String, LineageNode>,
    edges: Vec<LineageEdge>,
    outgoing: BTreeMap<String, Vec<usize>>,
    incoming: BTreeMap<String, Vec<usize>>,
    aliases: BTreeMap<String, String>,
}

impl LineageGraph {
    pub fn build(
        root: &NotebookRoot,
        entries: &[DatasourceEntry],
        port_versions: &BTreeMap<String, u64>,
    ) -> Self {
        let mut graph = Self::default();

        for entry in entries {
            let node_ref = Ref::Datasource {
                id: datasource_id(entry, entries),
                table: None,
            }
            .to_string();
            graph.add_node(LineageNode::dataset(node_ref.clone(), STATE_FRESH));
            graph.aliases.insert(node_ref.clone(), node_ref);
        }

        for cell in &root.cells {
            let Some(cell_id) = cell_id(cell) else {
                continue;
            };
            let cell_ref = cell_ref(cell).unwrap_or_else(|| {
                Ref::Cell {
                    id: cell_id.clone(),
                    version: None,
                }
                .to_string()
            });
            graph.aliases.insert(
                Ref::Cell {
                    id: cell_id.clone(),
                    version: None,
                }
                .to_string(),
                cell_ref.clone(),
            );
            graph.add_node(LineageNode::job(
                cell_ref.clone(),
                cell_execution_count(cell),
                cell_error_excerpt(cell),
            ));

            if let Some(dag) = cell_dag(cell) {
                graph.add_dag_edges(&cell_ref, dag, entries, port_versions);
            }

            if let Some(frontend) = cell_frontend(cell) {
                graph.add_frontend_edges(&cell_ref, frontend, port_versions);
            }
        }

        graph
    }

    pub fn walk(&self, start: &Ref, direction: Direction, depth: usize) -> LineageWalk {
        let root = self.resolve_ref(start).unwrap_or_else(|| start.to_string());
        if !self.nodes.contains_key(&root) {
            return LineageWalk {
                root,
                nodes: Vec::new(),
                edges: Vec::new(),
                truncated: false,
            };
        }

        let mut visited = BTreeSet::from([root.clone()]);
        let mut included_edges = BTreeSet::<usize>::new();
        let mut queue = VecDeque::from([(root.clone(), 0usize)]);
        let mut truncated = false;

        while let Some((node_ref, distance)) = queue.pop_front() {
            let neighbors = self.neighbors(&node_ref, direction);
            if distance >= depth {
                if !neighbors.is_empty() {
                    truncated = true;
                }
                continue;
            }

            for (edge_index, neighbor) in neighbors {
                let first_edge_visit = included_edges.insert(edge_index);
                if visited.contains(&neighbor) {
                    if first_edge_visit {
                        truncated = true;
                    }
                    continue;
                }
                if visited.len() >= VISITED_NODE_CAP {
                    truncated = true;
                    continue;
                }
                visited.insert(neighbor.clone());
                queue.push_back((neighbor, distance + 1));
            }
        }

        LineageWalk {
            root,
            nodes: visited
                .iter()
                .filter_map(|node_ref| self.nodes.get(node_ref).cloned())
                .collect(),
            edges: included_edges
                .into_iter()
                .filter_map(|index| self.edges.get(index).cloned())
                .collect(),
            truncated,
        }
    }

    fn add_dag_edges(
        &mut self,
        cell_ref: &str,
        dag: &CellDagMetadata,
        entries: &[DatasourceEntry],
        port_versions: &BTreeMap<String, u64>,
    ) {
        if let Some(source) = &dag.source {
            for entry in entries {
                if source.kind == datasource_kind_name(entry.kind) && source.port == entry.name {
                    let source_ref = Ref::Datasource {
                        id: datasource_id(entry, entries),
                        table: None,
                    }
                    .to_string();
                    self.add_node(LineageNode::dataset(source_ref.clone(), STATE_FRESH));
                    self.add_edge(source_ref, cell_ref.to_string(), "source");
                }
            }
        }

        for produced in &dag.produces {
            let port_ref = self.port_ref(&produced.port, port_versions);
            self.add_port_node(&produced.port, port_versions);
            self.add_edge(cell_ref.to_string(), port_ref, "produces");
        }

        for consumed in &dag.consumes {
            let port_ref = self.port_ref(consumed, port_versions);
            self.add_port_node(consumed, port_versions);
            self.add_edge(port_ref, cell_ref.to_string(), "consumes");
        }
    }

    fn add_frontend_edges(
        &mut self,
        cell_ref: &str,
        frontend: &FrontendCellMetadata,
        port_versions: &BTreeMap<String, u64>,
    ) {
        for bound in &frontend.binds {
            let port_ref = self.port_ref(bound, port_versions);
            self.add_port_node(bound, port_versions);
            self.add_edge(port_ref, cell_ref.to_string(), "binds");
        }

        for emitted in &frontend.emits {
            let port_ref = self.port_ref(emitted, port_versions);
            self.add_port_node(emitted, port_versions);
            self.add_edge(cell_ref.to_string(), port_ref, "emits");
        }
    }

    fn add_port_node(&mut self, name: &str, port_versions: &BTreeMap<String, u64>) {
        let port_ref = self.port_ref(name, port_versions);
        let state = if port_versions.contains_key(name) {
            STATE_FRESH
        } else {
            STATE_UNKNOWN
        };
        self.add_node(LineageNode::dataset(port_ref.clone(), state));
        self.aliases.insert(
            Ref::Port {
                name: name.to_string(),
                version: None,
            }
            .to_string(),
            port_ref,
        );
    }

    fn add_node(&mut self, node: LineageNode) {
        self.nodes.entry(node.r#ref.clone()).or_insert(node);
    }

    fn add_edge(&mut self, from: String, to: String, via: &'static str) {
        let edge = LineageEdge {
            from: from.clone(),
            to: to.clone(),
            via,
            provenance: PROVENANCE_DECLARED,
        };
        if self.edges.contains(&edge) {
            return;
        }
        let index = self.edges.len();
        self.edges.push(edge);
        self.outgoing.entry(from).or_default().push(index);
        self.incoming.entry(to).or_default().push(index);
    }

    fn neighbors(&self, node_ref: &str, direction: Direction) -> Vec<(usize, String)> {
        let mut neighbors = Vec::new();
        if matches!(direction, Direction::Downstream | Direction::Both) {
            neighbors.extend(
                self.outgoing
                    .get(node_ref)
                    .into_iter()
                    .flatten()
                    .filter_map(|index| Some((*index, self.edges.get(*index)?.to.clone()))),
            );
        }
        if matches!(direction, Direction::Upstream | Direction::Both) {
            neighbors.extend(
                self.incoming
                    .get(node_ref)
                    .into_iter()
                    .flatten()
                    .filter_map(|index| Some((*index, self.edges.get(*index)?.from.clone()))),
            );
        }
        neighbors.sort();
        neighbors
    }

    fn resolve_ref(&self, reference: &Ref) -> Option<String> {
        let raw = reference.to_string();
        if self.nodes.contains_key(&raw) {
            return Some(raw);
        }

        let alias = match reference {
            Ref::Cell { id, .. } => Ref::Cell {
                id: id.clone(),
                version: None,
            }
            .to_string(),
            Ref::Port { name, .. } => Ref::Port {
                name: name.clone(),
                version: None,
            }
            .to_string(),
            Ref::Datasource { .. } | Ref::Symbol { .. } => raw,
        };
        self.aliases.get(&alias).cloned()
    }

    fn port_ref(&self, name: &str, port_versions: &BTreeMap<String, u64>) -> String {
        Ref::Port {
            name: name.to_string(),
            version: port_versions.get(name).copied(),
        }
        .to_string()
    }
}

impl LineageNode {
    fn dataset(r#ref: String, state: &'static str) -> Self {
        Self {
            r#ref,
            role: ROLE_DATASET,
            state,
            execution_count: None,
            error_excerpt: None,
        }
    }

    fn job(r#ref: String, execution_count: Option<u32>, error_excerpt: Option<String>) -> Self {
        Self {
            r#ref,
            role: ROLE_JOB,
            state: if error_excerpt.is_some() {
                STATE_FAILED
            } else {
                STATE_UNKNOWN
            },
            execution_count,
            error_excerpt,
        }
    }
}

fn cell_ref(cell: &Cell) -> Option<String> {
    Some(
        Ref::Cell {
            id: cell_id(cell)?,
            version: cell_version(cell),
        }
        .to_string(),
    )
}

fn cell_id(cell: &Cell) -> Option<String> {
    match cell {
        Cell::Raw(cell) => cell.id.clone(),
        Cell::Markdown(cell) => cell.id.clone(),
        Cell::Code(cell) => cell.id.clone(),
    }
}

fn cell_version(cell: &Cell) -> Option<u64> {
    match cell {
        Cell::Raw(cell) => cell.metadata.spur.as_ref().map(|spur| spur.version),
        Cell::Markdown(cell) => cell.metadata.spur.as_ref().map(|spur| spur.version),
        Cell::Code(cell) => cell.metadata.spur.as_ref().map(|spur| spur.version),
    }
}

fn cell_execution_count(cell: &Cell) -> Option<u32> {
    match cell {
        Cell::Code(cell) => cell.execution_count,
        Cell::Raw(_) | Cell::Markdown(_) => None,
    }
}

fn cell_dag(cell: &Cell) -> Option<&CellDagMetadata> {
    match cell {
        Cell::Raw(cell) => cell.metadata.spur.as_ref()?.dag.as_ref(),
        Cell::Markdown(cell) => cell.metadata.spur.as_ref()?.dag.as_ref(),
        Cell::Code(cell) => cell.metadata.spur.as_ref()?.dag.as_ref(),
    }
}

fn cell_frontend(cell: &Cell) -> Option<&FrontendCellMetadata> {
    match cell {
        Cell::Raw(cell) => cell.metadata.spur.as_ref()?.frontend.as_ref(),
        Cell::Markdown(cell) => cell.metadata.spur.as_ref()?.frontend.as_ref(),
        Cell::Code(cell) => cell.metadata.spur.as_ref()?.frontend.as_ref(),
    }
}

fn cell_error_excerpt(cell: &Cell) -> Option<String> {
    let Cell::Code(cell) = cell else {
        return None;
    };
    cell.outputs.iter().find_map(|output| {
        let Output::Error(error) = output else {
            return None;
        };
        Some(truncate_chars(
            &format!("{}: {}", error.ename, error.evalue),
            ERROR_EXCERPT_CAP,
        ))
    })
}

fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}

fn datasource_kind_name(kind: DatasourceKind) -> &'static str {
    match kind {
        DatasourceKind::Csv => "csv",
        DatasourceKind::Parquet => "parquet",
        DatasourceKind::Json => "json",
        DatasourceKind::DuckDb => "duck_db",
        DatasourceKind::Sqlite => "sqlite",
        DatasourceKind::ApiTables => "api_tables",
    }
}
