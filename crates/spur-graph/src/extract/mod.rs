pub mod languages;
pub mod tree_sitter;

pub use tree_sitter::extract_rust_worktree;

use crate::{GraphEdge, GraphNode, SourceSpan};

#[derive(Debug, Clone, PartialEq)]
pub struct GraphFacts {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    pub spans: Vec<SourceSpan>,
}

impl GraphFacts {
    pub fn empty() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            spans: Vec::new(),
        }
    }
}
