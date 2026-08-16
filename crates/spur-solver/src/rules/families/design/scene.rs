//! Typed geometry facts and bounded unknowns for the design rule family.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::{MAX_CONSTRAINTS, MAX_VARIABLES};

/// Maximum scene nodes accepted before family-specific compilation.
pub const MAX_DESIGN_NODES: usize = MAX_CONSTRAINTS;
/// Maximum geometry unknowns accepted before family-specific compilation.
pub const MAX_DESIGN_UNKNOWNS: usize = MAX_VARIABLES;

/// A normalized design scene.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesignScene {
    /// Concrete viewport dimensions.
    pub viewport: DesignSize,
    /// Stable node IDs mapped to their geometry facts.
    pub nodes: BTreeMap<String, DesignNode>,
}

impl DesignScene {
    /// Validates geometry, parent topology, and unknown coverage.
    pub fn validate(&self, unknowns: &[DesignUnknown]) -> Result<(), DesignSceneError> {
        if self.viewport.width <= 0 || self.viewport.height <= 0 {
            return Err(DesignSceneError::NonPositiveViewport {
                width: self.viewport.width,
                height: self.viewport.height,
            });
        }
        if self.nodes.len() > MAX_DESIGN_NODES {
            return Err(DesignSceneError::TooManyNodes {
                count: self.nodes.len(),
                max: MAX_DESIGN_NODES,
            });
        }
        if unknowns.len() > MAX_DESIGN_UNKNOWNS {
            return Err(DesignSceneError::TooManyUnknowns {
                count: unknowns.len(),
                max: MAX_DESIGN_UNKNOWNS,
            });
        }

        self.validate_concrete_dimensions()?;
        self.validate_parents()?;
        let unknown_paths = self.validate_unknowns(unknowns)?;
        self.validate_geometry_coverage(&unknown_paths)
    }

    fn validate_concrete_dimensions(&self) -> Result<(), DesignSceneError> {
        for (node_id, node) in &self.nodes {
            for field in [DesignField::Width, DesignField::Height] {
                if let Some(value) = node.rect.value(field) {
                    if value <= 0 {
                        return Err(DesignSceneError::NonPositiveDimension {
                            node: node_id.clone(),
                            field,
                            value,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_parents(&self) -> Result<(), DesignSceneError> {
        for (node_id, node) in &self.nodes {
            if let Some(parent) = &node.parent {
                if !self.nodes.contains_key(parent) {
                    return Err(DesignSceneError::UnknownParent {
                        node: node_id.clone(),
                        parent: parent.clone(),
                    });
                }
            }
        }

        for start in self.nodes.keys() {
            let mut seen = BTreeSet::new();
            let mut cursor = Some(start.as_str());
            while let Some(node_id) = cursor {
                if !seen.insert(node_id) {
                    return Err(DesignSceneError::ParentCycle {
                        node: start.clone(),
                    });
                }
                cursor = self.nodes[node_id].parent.as_deref();
            }
        }
        Ok(())
    }

    fn validate_unknowns(
        &self,
        unknowns: &[DesignUnknown],
    ) -> Result<BTreeSet<(String, DesignField)>, DesignSceneError> {
        let mut paths = BTreeSet::new();
        for unknown in unknowns {
            let Some(node) = self.nodes.get(&unknown.node) else {
                return Err(DesignSceneError::UnknownNode {
                    node: unknown.node.clone(),
                });
            };
            if unknown.min > unknown.max {
                return Err(DesignSceneError::InvalidUnknownRange {
                    node: unknown.node.clone(),
                    field: unknown.field,
                    min: unknown.min,
                    max: unknown.max,
                });
            }
            if matches!(unknown.field, DesignField::Width | DesignField::Height) && unknown.min <= 0
            {
                return Err(DesignSceneError::NonPositiveDimensionRange {
                    node: unknown.node.clone(),
                    field: unknown.field,
                    min: unknown.min,
                });
            }
            if node.rect.value(unknown.field).is_some() {
                return Err(DesignSceneError::UnknownHasConcreteValue {
                    node: unknown.node.clone(),
                    field: unknown.field,
                });
            }
            if !paths.insert((unknown.node.clone(), unknown.field)) {
                return Err(DesignSceneError::DuplicateUnknown {
                    node: unknown.node.clone(),
                    field: unknown.field,
                });
            }
        }
        Ok(paths)
    }

    fn validate_geometry_coverage(
        &self,
        unknown_paths: &BTreeSet<(String, DesignField)>,
    ) -> Result<(), DesignSceneError> {
        for (node_id, node) in &self.nodes {
            for field in DesignField::ALL {
                if node.rect.value(field).is_none()
                    && !unknown_paths.contains(&(node_id.clone(), field))
                {
                    return Err(DesignSceneError::MissingGeometry {
                        node: node_id.clone(),
                        field,
                    });
                }
            }
        }
        Ok(())
    }
}

/// Concrete viewport dimensions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesignSize {
    /// Width in normalized integer units.
    pub width: i64,
    /// Height in normalized integer units.
    pub height: i64,
}

/// One scene node and optional parent relation.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesignNode {
    /// Parent node ID when the node participates in a hierarchy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Rectangle facts; omitted fields must have matching unknown declarations.
    pub rect: DesignRect,
}

/// Rectangle geometry. Coordinates may be negative; dimensions must be positive.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesignRect {
    /// Left coordinate, or `None` when declared as an unknown.
    pub x: Option<i64>,
    /// Top coordinate, or `None` when declared as an unknown.
    pub y: Option<i64>,
    /// Positive width, or `None` when declared as an unknown.
    pub width: Option<i64>,
    /// Positive height, or `None` when declared as an unknown.
    pub height: Option<i64>,
}

impl DesignRect {
    /// Returns the concrete value for one field.
    #[must_use]
    pub const fn value(self, field: DesignField) -> Option<i64> {
        match field {
            DesignField::X => self.x,
            DesignField::Y => self.y,
            DesignField::Width => self.width,
            DesignField::Height => self.height,
        }
    }
}

/// One addressable rectangle field.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DesignField {
    /// Left coordinate.
    X,
    /// Top coordinate.
    Y,
    /// Width.
    Width,
    /// Height.
    Height,
}

impl DesignField {
    /// Stable field iteration order used by validation and compilation.
    pub const ALL: [Self; 4] = [Self::X, Self::Y, Self::Width, Self::Height];
}

/// One bounded geometry variable.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DesignUnknown {
    /// Existing scene node whose field is unknown.
    pub node: String,
    /// Rectangle field replaced by the variable.
    pub field: DesignField,
    /// Inclusive lower bound.
    pub min: i64,
    /// Inclusive upper bound.
    pub max: i64,
}

/// Deterministic scene validation failures.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum DesignSceneError {
    /// The viewport cannot define a usable coordinate space.
    #[error("viewport dimensions must be positive, got {width}x{height}")]
    NonPositiveViewport { width: i64, height: i64 },
    /// Scene size exceeds the inherited backend budget.
    #[error("scene has {count} nodes; maximum is {max}")]
    TooManyNodes { count: usize, max: usize },
    /// Unknown count exceeds the inherited backend variable budget.
    #[error("scene has {count} unknowns; maximum is {max}")]
    TooManyUnknowns { count: usize, max: usize },
    /// A concrete width or height is invalid.
    #[error("node `{node}` field `{field:?}` must be positive, got {value}")]
    NonPositiveDimension {
        node: String,
        field: DesignField,
        value: i64,
    },
    /// A parent ID does not resolve.
    #[error("node `{node}` references unknown parent `{parent}`")]
    UnknownParent { node: String, parent: String },
    /// Parent relations contain a cycle.
    #[error("parent cycle reachable from node `{node}`")]
    ParentCycle { node: String },
    /// An unknown references a missing node.
    #[error("unknown geometry references missing node `{node}`")]
    UnknownNode { node: String },
    /// An unknown has reversed bounds.
    #[error("unknown `{node}.{field:?}` has invalid range {min}..={max}")]
    InvalidUnknownRange {
        node: String,
        field: DesignField,
        min: i64,
        max: i64,
    },
    /// A dimension unknown could include zero or a negative value.
    #[error("unknown dimension `{node}.{field:?}` must have a positive minimum, got {min}")]
    NonPositiveDimensionRange {
        node: String,
        field: DesignField,
        min: i64,
    },
    /// A declared unknown conflicts with a concrete scene fact.
    #[error("unknown `{node}.{field:?}` already has a concrete value")]
    UnknownHasConcreteValue { node: String, field: DesignField },
    /// The same unknown path was declared more than once.
    #[error("unknown `{node}.{field:?}` is declared more than once")]
    DuplicateUnknown { node: String, field: DesignField },
    /// An omitted rectangle field has no variable declaration.
    #[error("node `{node}` field `{field:?}` is missing and has no unknown declaration")]
    MissingGeometry { node: String, field: DesignField },
}
