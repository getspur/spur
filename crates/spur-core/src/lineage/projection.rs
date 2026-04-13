use std::collections::HashMap;

use spur_acp::SpurEvent;

use super::types::{ExecutorId, ExecutorNode};

/// Event-sourced projection of executor lineage.
#[derive(Debug, Default, Clone)]
pub struct ExecutorLineage {
    nodes: HashMap<ExecutorId, ExecutorNode>,
    roots: Vec<ExecutorId>,
}

impl ExecutorLineage {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one event into the projection. No-op in the stub.
    pub fn apply(&mut self, _event: &SpurEvent) {
        // implemented in later tasks
    }

    pub fn nodes(&self) -> impl Iterator<Item = &ExecutorNode> {
        self.nodes.values()
    }

    pub fn node(&self, id: &ExecutorId) -> Option<&ExecutorNode> {
        self.nodes.get(id)
    }

    pub fn root_ids(&self) -> &[ExecutorId] {
        &self.roots
    }

    pub fn children_of(&self, id: &ExecutorId) -> Vec<&ExecutorNode> {
        match self.nodes.get(id) {
            Some(node) => node
                .child_ids
                .iter()
                .filter_map(|cid| self.nodes.get(cid))
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn pending_reviews(&self) -> Vec<ExecutorId> {
        self.nodes
            .values()
            .filter(|n| n.pending_review.is_some())
            .map(|n| n.id.clone())
            .collect()
    }

    #[allow(dead_code)]
    pub(crate) fn insert_root(&mut self, node: ExecutorNode) {
        self.roots.push(node.id.clone());
        self.nodes.insert(node.id.clone(), node);
    }

    #[allow(dead_code)]
    pub(crate) fn insert_child(&mut self, parent: &ExecutorId, node: ExecutorNode) {
        if let Some(p) = self.nodes.get_mut(parent) {
            p.child_ids.push(node.id.clone());
        }
        self.nodes.insert(node.id.clone(), node);
    }

    #[allow(dead_code)]
    pub(crate) fn node_mut(&mut self, id: &ExecutorId) -> Option<&mut ExecutorNode> {
        self.nodes.get_mut(id)
    }
}
