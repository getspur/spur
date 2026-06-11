use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt,
};

use jute::backend::notebook::{Cell, CellDagMetadata, DagSource, NotebookRoot, Output};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DagEdge {
    pub producer: String,
    pub consumer: String,
    pub port: String,
}

impl DagEdge {
    pub fn new(
        producer: impl Into<String>,
        consumer: impl Into<String>,
        port: impl Into<String>,
    ) -> Self {
        Self {
            producer: producer.into(),
            consumer: consumer.into(),
            port: port.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DagError {
    DuplicateProducer {
        port: String,
        first_cell: String,
        second_cell: String,
    },
    Cycle {
        ports: Vec<String>,
    },
}

impl fmt::Display for DagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateProducer {
                port,
                first_cell,
                second_cell,
            } => write!(
                f,
                "port '{port}' is produced by both '{first_cell}' and '{second_cell}'"
            ),
            Self::Cycle { ports } => {
                write!(f, "cycle detected through port(s): {}", ports.join(", "))
            }
        }
    }
}

impl Error for DagError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SourcePortError {
    NotDeclared { port: String },
    Ambiguous { port: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WidgetEmitError {
    ModelNotFound {
        model_id: String,
    },
    CellMissingFrontendMetadata {
        model_id: String,
        cell_id: String,
    },
    EmitNotDeclared {
        model_id: String,
        cell_id: String,
        port: String,
    },
}

pub(crate) fn resolve_source_for_port(
    root: &NotebookRoot,
    port: &str,
) -> Result<DagSource, SourcePortError> {
    let mut matches = root
        .cells
        .iter()
        .filter_map(cell_dag_source)
        .filter(|source| source.port == port);
    let Some(source) = matches.next().cloned() else {
        return Err(SourcePortError::NotDeclared {
            port: port.to_owned(),
        });
    };
    if matches.any(|other| other.kind != source.kind) {
        return Err(SourcePortError::Ambiguous {
            port: port.to_owned(),
        });
    }
    Ok(source)
}

pub(crate) fn resolve_source_cell_for_port(
    root: &NotebookRoot,
    port: &str,
) -> Result<(String, DagSource), SourcePortError> {
    let mut matches = root
        .cells
        .iter()
        .filter_map(|cell| Some((cell_id(cell)?.to_owned(), cell_dag_source(cell)?)))
        .filter(|(_, source)| source.port == port);
    let Some((cell_id, source)) = matches.next() else {
        return Err(SourcePortError::NotDeclared {
            port: port.to_owned(),
        });
    };
    if matches.any(|(_, other)| other.kind != source.kind) {
        return Err(SourcePortError::Ambiguous {
            port: port.to_owned(),
        });
    }
    Ok((cell_id, source.clone()))
}

pub(crate) fn resolve_widget_emit_cell(
    root: &NotebookRoot,
    model_id: &str,
    port: &str,
) -> Result<String, WidgetEmitError> {
    let Some(cell) = root
        .cells
        .iter()
        .find(|cell| cell_has_widget_model_id(cell, model_id))
    else {
        return Err(WidgetEmitError::ModelNotFound {
            model_id: model_id.to_owned(),
        });
    };
    let cell_id = cell_id(cell).unwrap_or_default().to_owned();
    let Some(frontend) = cell_frontend_metadata(cell) else {
        return Err(WidgetEmitError::CellMissingFrontendMetadata {
            model_id: model_id.to_owned(),
            cell_id,
        });
    };
    if frontend.emits.iter().any(|declared| declared == port) {
        return Ok(cell_id);
    }
    Err(WidgetEmitError::EmitNotDeclared {
        model_id: model_id.to_owned(),
        cell_id,
        port: port.to_owned(),
    })
}

fn cell_id(cell: &Cell) -> Option<&str> {
    match cell {
        Cell::Raw(cell) => cell.id.as_deref(),
        Cell::Markdown(cell) => cell.id.as_deref(),
        Cell::Code(cell) => cell.id.as_deref(),
    }
}

fn cell_frontend_metadata(cell: &Cell) -> Option<&jute::backend::notebook::FrontendCellMetadata> {
    match cell {
        Cell::Raw(cell) => cell.metadata.spur.as_ref()?.frontend.as_ref(),
        Cell::Markdown(cell) => cell.metadata.spur.as_ref()?.frontend.as_ref(),
        Cell::Code(cell) => cell.metadata.spur.as_ref()?.frontend.as_ref(),
    }
}

fn cell_dag_source(cell: &Cell) -> Option<&DagSource> {
    match cell {
        Cell::Raw(cell) => cell.metadata.spur.as_ref()?.dag.as_ref()?.source.as_ref(),
        Cell::Markdown(cell) => cell.metadata.spur.as_ref()?.dag.as_ref()?.source.as_ref(),
        Cell::Code(cell) => cell.metadata.spur.as_ref()?.dag.as_ref()?.source.as_ref(),
    }
}

fn cell_has_widget_model_id(cell: &Cell, model_id: &str) -> bool {
    let Cell::Code(cell) = cell else {
        return false;
    };
    cell.outputs.iter().filter_map(output_data).any(|data| {
        data.get("application/vnd.jupyter.widget-view+json")
            .is_some_and(|value| {
                value.get("model_id").and_then(|value| value.as_str()) == Some(model_id)
            })
    })
}

fn output_data(output: &Output) -> Option<&jute::backend::notebook::MimeBundle> {
    match output {
        Output::ExecuteResult(output) => Some(&output.data),
        Output::DisplayData(output) => Some(&output.data),
        Output::Stream(_) | Output::Error(_) => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SourceKey {
    kind: String,
    port: String,
}

impl From<&DagSource> for SourceKey {
    fn from(source: &DagSource) -> Self {
        Self {
            kind: source.kind.clone(),
            port: source.port.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NotebookDag {
    nodes: BTreeMap<String, CellDagMetadata>,
    producers_by_port: BTreeMap<String, String>,
    consumers_by_port: BTreeMap<String, BTreeSet<String>>,
    sources_by_key: BTreeMap<SourceKey, BTreeSet<String>>,
}

impl NotebookDag {
    pub fn from_metadata(
        metadata_by_cell: impl IntoIterator<Item = (String, CellDagMetadata)>,
    ) -> Result<Self, DagError> {
        let nodes: BTreeMap<String, CellDagMetadata> = metadata_by_cell.into_iter().collect();
        let mut producers_by_port: BTreeMap<String, String> = BTreeMap::new();
        let mut consumers_by_port: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut sources_by_key: BTreeMap<SourceKey, BTreeSet<String>> = BTreeMap::new();

        for (cell_id, metadata) in &nodes {
            for produced in &metadata.produces {
                if let Some(first_cell) = producers_by_port.get(&produced.port) {
                    return Err(DagError::DuplicateProducer {
                        port: produced.port.clone(),
                        first_cell: first_cell.clone(),
                        second_cell: cell_id.clone(),
                    });
                }
                producers_by_port.insert(produced.port.clone(), cell_id.clone());
            }

            for consumed in &metadata.consumes {
                consumers_by_port
                    .entry(consumed.clone())
                    .or_default()
                    .insert(cell_id.clone());
            }

            if let Some(source) = &metadata.source {
                sources_by_key
                    .entry(SourceKey::from(source))
                    .or_default()
                    .insert(cell_id.clone());
            }
        }

        let dag = Self {
            nodes,
            producers_by_port,
            consumers_by_port,
            sources_by_key,
        };
        dag.topological_sort()?;
        Ok(dag)
    }

    pub fn cell_metadata(&self, cell_id: &str) -> Option<&CellDagMetadata> {
        self.nodes.get(cell_id)
    }

    pub fn consumed_ports(&self, cell_id: &str) -> Vec<String> {
        self.nodes
            .get(cell_id)
            .map(|metadata| metadata.consumes.clone())
            .unwrap_or_default()
    }

    pub fn producer_for_port(&self, port: &str) -> Option<&str> {
        self.producers_by_port.get(port).map(String::as_str)
    }

    pub fn declared_schema_for_port(&self, port: &str) -> Option<&Value> {
        self.nodes.values().find_map(|metadata| {
            metadata
                .produces
                .iter()
                .find(|produced| produced.port == port)
                .and_then(|produced| produced.schema.as_ref())
                .or_else(|| {
                    metadata
                        .source
                        .as_ref()
                        .filter(|source| source.port == port)
                        .and_then(|source| source.schema.as_ref())
                })
        })
    }

    pub fn edges(&self) -> Vec<DagEdge> {
        self.consumers_by_port
            .iter()
            .filter_map(|(port, consumers)| {
                self.producers_by_port
                    .get(port)
                    .map(|producer| (port, producer, consumers))
            })
            .flat_map(|(port, producer, consumers)| {
                consumers
                    .iter()
                    .map(move |consumer| DagEdge::new(producer, consumer, port))
            })
            .collect()
    }

    pub fn topological_sort(&self) -> Result<Vec<String>, DagError> {
        // Build the producer -> {consumer} adjacency first, collapsing parallel
        // edges. `edges()` yields one edge per (port, consumer), so a producer
        // that feeds several ports to the SAME consumer shows up multiple times
        // even though it is a single scheduling dependency. Derive `indegree`
        // from this deduped adjacency so the Kahn drain decrements exactly as
        // many times as it incremented; counting indegree per raw edge while
        // the adjacency is a set under-decrements multi-port pairs and reports
        // a spurious cycle.
        let mut outgoing: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for edge in self.edges() {
            outgoing
                .entry(edge.producer)
                .or_default()
                .insert(edge.consumer);
        }

        let mut indegree = self
            .nodes
            .keys()
            .map(|cell_id| (cell_id.clone(), 0usize))
            .collect::<BTreeMap<_, _>>();
        for consumers in outgoing.values() {
            for consumer in consumers {
                if let Some(count) = indegree.get_mut(consumer) {
                    *count += 1;
                }
            }
        }

        let mut ready = indegree
            .iter()
            .filter_map(|(cell_id, count)| (*count == 0).then(|| cell_id.clone()))
            .collect::<BTreeSet<_>>();
        let mut ordered = Vec::with_capacity(self.nodes.len());

        while let Some(cell_id) = ready.pop_first() {
            ordered.push(cell_id.clone());

            for consumer in outgoing.get(&cell_id).into_iter().flatten() {
                let count = indegree
                    .get_mut(consumer)
                    .expect("edge consumers are known nodes");
                *count -= 1;
                if *count == 0 {
                    ready.insert(consumer.clone());
                }
            }
        }

        if ordered.len() == self.nodes.len() {
            return Ok(ordered);
        }

        Err(DagError::Cycle {
            ports: self.cycle_ports(&ordered),
        })
    }

    pub fn stale_from_port(&self, port: &str) -> Result<Vec<String>, DagError> {
        let seeds = self
            .consumers_by_port
            .get(port)
            .cloned()
            .unwrap_or_default();
        self.ordered_reachable(seeds)
    }

    pub fn stale_from_source(&self, source: &DagSource) -> Result<Vec<String>, DagError> {
        let seeds = self
            .sources_by_key
            .get(&SourceKey::from(source))
            .cloned()
            .unwrap_or_default();
        self.ordered_reachable(seeds)
    }

    fn ordered_reachable(&self, seeds: BTreeSet<String>) -> Result<Vec<String>, DagError> {
        let mut reachable = seeds.clone();
        let mut queue = VecDeque::from_iter(seeds);

        while let Some(cell_id) = queue.pop_front() {
            for downstream in self.downstream_cells(&cell_id) {
                if reachable.insert(downstream.clone()) {
                    queue.push_back(downstream);
                }
            }
        }

        Ok(self
            .topological_sort()?
            .into_iter()
            .filter(|cell_id| reachable.contains(cell_id))
            .collect())
    }

    fn downstream_cells(&self, cell_id: &str) -> Vec<String> {
        self.nodes
            .get(cell_id)
            .into_iter()
            .flat_map(|metadata| &metadata.produces)
            .flat_map(|produced| {
                self.consumers_by_port
                    .get(&produced.port)
                    .into_iter()
                    .flatten()
                    .cloned()
            })
            .collect()
    }

    fn cycle_ports(&self, ordered: &[String]) -> Vec<String> {
        let sorted = ordered.iter().collect::<BTreeSet<_>>();
        let remaining = self
            .nodes
            .keys()
            .filter(|cell_id| !sorted.contains(cell_id))
            .collect::<BTreeSet<_>>();

        self.edges()
            .into_iter()
            .filter(|edge| remaining.contains(&edge.producer) && remaining.contains(&edge.consumer))
            .map(|edge| edge.port)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use jute::backend::notebook::{
        Cell, CellDagMetadata, CellMetadata, CodeCell, DagSource, MultilineString,
        NotebookMetadata, NotebookRoot, PortSpec, SpurCellMetadata,
    };

    use super::{resolve_source_for_port, DagEdge, DagError, NotebookDag, SourcePortError};

    fn port(name: &str) -> PortSpec {
        PortSpec {
            port: name.to_string(),
            repr: "dataframe".to_string(),
            display: None,
            class: None,
            schema: None,
        }
    }

    fn source(port: &str) -> DagSource {
        DagSource {
            kind: "datasource".to_string(),
            port: port.to_string(),
            class: None,
            schema: None,
        }
    }

    fn source_with_kind(kind: &str, port: &str) -> DagSource {
        DagSource {
            kind: kind.to_string(),
            port: port.to_string(),
            class: None,
            schema: None,
        }
    }

    fn cell(
        produces: impl IntoIterator<Item = &'static str>,
        consumes: impl IntoIterator<Item = &'static str>,
        source: Option<DagSource>,
    ) -> CellDagMetadata {
        CellDagMetadata {
            produces: produces.into_iter().map(port).collect(),
            consumes: consumes.into_iter().map(str::to_string).collect(),
            source,
        }
    }

    fn graph(cells: Vec<(&str, CellDagMetadata)>) -> NotebookDag {
        NotebookDag::from_metadata(
            cells
                .into_iter()
                .map(|(id, metadata)| (id.to_string(), metadata))
                .collect::<BTreeMap<_, _>>(),
        )
        .expect("graph builds")
    }

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

    fn source_cell(id: &str, source: DagSource) -> Cell {
        Cell::Code(CodeCell {
            id: Some(id.to_string()),
            metadata: CellMetadata {
                spur: Some(SpurCellMetadata {
                    version: 1,
                    last_edited_by: None,
                    datasource_setup: None,
                    dag: Some(CellDagMetadata {
                        produces: Vec::new(),
                        consumes: Vec::new(),
                        source: Some(source),
                    }),
                    code_type: None,
                    frontend: None,
                }),
                jute_deck: None,
                other: Default::default(),
            },
            source: MultilineString::Single("print('ok')".to_string()),
            execution_count: None,
            outputs: Vec::new(),
        })
    }

    #[test]
    fn resolve_source_for_port_rejects_undeclared_port() {
        let error = resolve_source_for_port(&notebook(Vec::new()), "sales")
            .expect_err("missing source port is rejected");

        assert_eq!(
            error,
            SourcePortError::NotDeclared {
                port: "sales".to_string()
            }
        );
    }

    #[test]
    fn resolve_source_for_port_returns_single_matching_source() {
        let expected = source("sales");
        let root = notebook(vec![source_cell("source", expected.clone())]);

        let resolved = resolve_source_for_port(&root, "sales").expect("source resolves");

        assert_eq!(resolved, expected);
    }

    #[test]
    fn resolve_source_for_port_rejects_ambiguous_source_kinds() {
        let root = notebook(vec![
            source_cell("source-a", source_with_kind("datasource", "sales")),
            source_cell("source-b", source_with_kind("stream", "sales")),
        ]);

        let error =
            resolve_source_for_port(&root, "sales").expect_err("ambiguous source port is rejected");

        assert_eq!(
            error,
            SourcePortError::Ambiguous {
                port: "sales".to_string()
            }
        );
    }

    #[test]
    fn diamond_graph_derives_edges_and_orders_stale_set() {
        let dag = graph(vec![
            ("root", cell(["raw"], [], Some(source("sales")))),
            ("left", cell(["left"], ["raw"], None)),
            ("right", cell(["right"], ["raw"], None)),
            ("join", cell(["joined"], ["left", "right"], None)),
        ]);

        assert_eq!(
            dag.edges(),
            vec![
                DagEdge::new("left", "join", "left"),
                DagEdge::new("root", "left", "raw"),
                DagEdge::new("root", "right", "raw"),
                DagEdge::new("right", "join", "right"),
            ]
        );
        assert_eq!(
            dag.topological_sort().unwrap(),
            ["root", "left", "right", "join"]
        );
        assert_eq!(
            dag.stale_from_port("raw").unwrap(),
            ["left", "right", "join"]
        );
        assert_eq!(
            dag.stale_from_source(&source("sales")).unwrap(),
            ["root", "left", "right", "join"]
        );
    }

    #[test]
    fn cycle_detection_names_offending_ports() {
        let error = NotebookDag::from_metadata(BTreeMap::from([
            ("a".to_string(), cell(["a_out"], ["c_out"], None)),
            ("b".to_string(), cell(["b_out"], ["a_out"], None)),
            ("c".to_string(), cell(["c_out"], ["b_out"], None)),
        ]))
        .expect_err("cycle is rejected");

        assert_eq!(
            error,
            DagError::Cycle {
                ports: vec![
                    "a_out".to_string(),
                    "b_out".to_string(),
                    "c_out".to_string()
                ],
            }
        );
    }

    #[test]
    fn duplicate_producers_are_rejected() {
        let error = NotebookDag::from_metadata(BTreeMap::from([
            ("a".to_string(), cell(["shared"], [], None)),
            ("b".to_string(), cell(["shared"], [], None)),
        ]))
        .expect_err("duplicate producer is rejected");

        assert_eq!(
            error,
            DagError::DuplicateProducer {
                port: "shared".to_string(),
                first_cell: "a".to_string(),
                second_cell: "b".to_string(),
            }
        );
    }

    #[test]
    fn multi_consumer_fan_out_keeps_stale_order_deterministic() {
        let dag = graph(vec![
            ("producer", cell(["raw"], [], None)),
            ("alpha", cell(["alpha"], ["raw"], None)),
            ("beta", cell(["beta"], ["raw"], None)),
            ("gamma", cell(["gamma"], ["raw"], None)),
        ]);

        assert_eq!(
            dag.stale_from_port("raw").unwrap(),
            ["alpha", "beta", "gamma"]
        );
    }

    #[test]
    fn independent_branches_stay_independent() {
        let dag = graph(vec![
            ("a0", cell(["a0"], [], None)),
            ("a1", cell(["a1"], ["a0"], None)),
            ("b0", cell(["b0"], [], None)),
            ("b1", cell(["b1"], ["b0"], None)),
        ]);

        assert_eq!(dag.stale_from_port("a0").unwrap(), ["a1"]);
        assert_eq!(dag.stale_from_port("b0").unwrap(), ["b1"]);
    }

    #[test]
    fn parallel_edges_same_pair_do_not_false_cycle() {
        // One producer feeding multiple ports to the SAME consumer is a single
        // scheduling dependency, not a cycle. Regression for the indegree vs.
        // adjacency multiplicity mismatch in `topological_sort`.
        let dag = graph(vec![
            ("producer", cell(["markets", "markets_agg"], [], None)),
            ("artifact", cell([], ["markets", "markets_agg"], None)),
        ]);

        assert_eq!(dag.topological_sort().unwrap(), ["producer", "artifact"]);
        assert_eq!(dag.stale_from_port("markets").unwrap(), ["artifact"]);
    }

    #[test]
    fn distinct_producers_both_ordered_before_consumer() {
        let dag = graph(vec![
            ("p1", cell(["one"], [], None)),
            ("p2", cell(["two"], [], None)),
            ("sink", cell([], ["one", "two"], None)),
        ]);

        let order = dag.topological_sort().unwrap();
        let pos = |id: &str| order.iter().position(|c| c == id).unwrap();
        assert_eq!(order.len(), 3);
        assert!(pos("p1") < pos("sink"));
        assert!(pos("p2") < pos("sink"));
    }

    #[test]
    fn self_dependency_is_cycle() {
        // A cell that consumes a port it produces depends on itself.
        let error = NotebookDag::from_metadata(BTreeMap::from([(
            "loop".to_string(),
            cell(["x"], ["x"], None),
        )]))
        .expect_err("self dependency is a cycle");

        assert_eq!(
            error,
            DagError::Cycle {
                ports: vec!["x".to_string()],
            }
        );
    }

    #[test]
    fn two_node_cycle_still_detected() {
        let error = NotebookDag::from_metadata(BTreeMap::from([
            ("a".to_string(), cell(["a_out"], ["b_out"], None)),
            ("b".to_string(), cell(["b_out"], ["a_out"], None)),
        ]))
        .expect_err("two node cycle is rejected");

        assert!(matches!(error, DagError::Cycle { .. }));
    }
}
