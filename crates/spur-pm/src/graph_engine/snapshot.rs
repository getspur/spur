use chrono::{DateTime, Utc};
use petgraph::graph::{Graph, NodeIndex};
use petgraph::Directed;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyKind {
    Blocks,
    ParentChild,
    ConditionalBlocks,
    WaitsFor,
    RelatedTo,
    Discovered,
    Unknown,
}

impl DependencyKind {
    pub fn parse(s: &str) -> Self {
        match s {
            "blocks" => Self::Blocks,
            "parent-child" => Self::ParentChild,
            "conditional-blocks" => Self::ConditionalBlocks,
            "waits-for" => Self::WaitsFor,
            "related-to" => Self::RelatedTo,
            "discovered" => Self::Discovered,
            _ => Self::Unknown,
        }
    }

    pub fn is_blocking(self) -> bool {
        matches!(
            self,
            Self::Blocks | Self::ParentChild | Self::ConditionalBlocks | Self::WaitsFor
        )
    }
}

#[derive(Debug, Clone)]
pub struct NodeData {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: i32,
    pub issue_type: String,
    pub assignee: Option<String>,
    pub labels: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub due_at: Option<DateTime<Utc>>,
    pub content_hash: String,
}

#[derive(Debug, Clone)]
pub struct EdgeData {
    pub kind: DependencyKind,
}

pub struct GraphSnapshot {
    pub graph: Graph<NodeData, EdgeData, Directed>,
    pub by_id: HashMap<String, NodeIndex>,
    pub generated_at: DateTime<Utc>,
    pub data_hash: String,
    pub label_filter: Option<String>,
}

impl GraphSnapshot {
    pub fn new(label_filter: Option<String>) -> Self {
        Self {
            graph: Graph::new(),
            by_id: HashMap::new(),
            generated_at: Utc::now(),
            data_hash: String::new(),
            label_filter,
        }
    }

    pub fn add_node(&mut self, data: NodeData) -> NodeIndex {
        let id = data.id.clone();
        let ix = self.graph.add_node(data);
        self.by_id.insert(id, ix);
        ix
    }

    pub fn add_edge(&mut self, from: &str, to: &str, kind: DependencyKind) -> bool {
        let (Some(&a), Some(&b)) = (self.by_id.get(from), self.by_id.get(to)) else {
            return false;
        };
        self.graph.add_edge(a, b, EdgeData { kind });
        true
    }

    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }

    pub fn edge_count(&self) -> usize {
        self.graph.edge_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str) -> NodeData {
        NodeData {
            id: id.to_string(),
            title: format!("Issue {id}"),
            status: "open".into(),
            priority: 2,
            issue_type: "task".into(),
            assignee: None,
            labels: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            due_at: None,
            content_hash: format!("hash-{id}"),
        }
    }

    #[test]
    fn add_node_and_edge() {
        let mut s = GraphSnapshot::new(None);
        s.add_node(node("a"));
        s.add_node(node("b"));
        assert_eq!(s.node_count(), 2);
        assert!(s.add_edge("a", "b", DependencyKind::Blocks));
        assert_eq!(s.edge_count(), 1);
    }

    #[test]
    fn add_edge_with_unknown_node_returns_false() {
        let mut s = GraphSnapshot::new(None);
        s.add_node(node("a"));
        assert!(!s.add_edge("a", "missing", DependencyKind::Blocks));
        assert_eq!(s.edge_count(), 0);
    }

    #[test]
    fn dependency_kind_parsing_and_is_blocking() {
        assert!(DependencyKind::parse("blocks").is_blocking());
        assert!(DependencyKind::parse("parent-child").is_blocking());
        assert!(DependencyKind::parse("conditional-blocks").is_blocking());
        assert!(DependencyKind::parse("waits-for").is_blocking());
        assert!(!DependencyKind::parse("related-to").is_blocking());
        assert!(!DependencyKind::parse("discovered").is_blocking());
        assert!(!DependencyKind::parse("garbage").is_blocking());
    }
}
