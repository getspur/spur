use std::collections::HashMap;

use anyhow::anyhow;
use petgraph::stable_graph::StableDiGraph;

use crate::extract::GraphFacts;
use crate::{GraphEdge, GraphNode, NodeId};

pub type OperationalGraph = StableDiGraph<GraphNode, GraphEdge>;

pub fn build_petgraph(facts: &GraphFacts) -> anyhow::Result<OperationalGraph> {
    let mut graph = StableDiGraph::new();
    let mut indices = HashMap::new();

    for node in &facts.nodes {
        let index = graph.add_node(node.clone());
        indices.insert(node.node_id, index);
    }

    for edge in &facts.edges {
        let source = lookup(&indices, edge.source_node_id)?;
        let target = lookup(&indices, edge.target_node_id)?;
        graph.add_edge(source, target, edge.clone());
    }

    Ok(graph)
}

fn lookup(
    indices: &HashMap<NodeId, petgraph::stable_graph::NodeIndex>,
    node_id: NodeId,
) -> anyhow::Result<petgraph::stable_graph::NodeIndex> {
    indices
        .get(&node_id)
        .copied()
        .ok_or_else(|| anyhow!("edge references missing node id {}", node_id.get()))
}
