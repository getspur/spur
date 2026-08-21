//! Geometry rules for UI layout and graphic design.

pub mod compile;
pub mod scene;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::rules::catalog::RuleRegistry;
use crate::rules::compiler::{
    FamilyCompilation, FamilyCompileError, ModelProjection, RuleFamilyCompiler,
};
use crate::rules::RuleSolveMode;
use crate::types::DEFAULT_TIMEOUT_MS;

use self::compile::{compile, DesignCompileRequest, DesignRuleBinding, MAX_DESIGN_RULE_BINDINGS};
use self::scene::{DesignScene, DesignUnknown, MAX_DESIGN_NODES, MAX_DESIGN_UNKNOWNS};

/// Design family compiler registered behind `solve_rules`.
pub static COMPILER: DesignCompiler = DesignCompiler;

/// Stateless adapter over the existing typed design compiler.
pub struct DesignCompiler;

impl RuleFamilyCompiler for DesignCompiler {
    fn id(&self) -> &'static str {
        "design"
    }

    fn input_schema(&self) -> Value {
        solve_rules_schema()
    }

    fn compile(&self, input: Value) -> Result<FamilyCompilation, FamilyCompileError> {
        let input: DesignToolRequest = serde_json::from_value(input)
            .map_err(|error| FamilyCompileError::new(self.id(), error.to_string()))?;
        let compiled = compile(DesignCompileRequest {
            mode: input.mode,
            rules: input.rules,
            scene: input.scene,
            unknowns: input.unknowns,
            timeout_ms: input.timeout_ms,
            persist: input.persist,
            include_smt: input.include_smt,
        })
        .map_err(|error| FamilyCompileError::new(self.id(), error.to_string()))?;

        Ok(FamilyCompilation {
            mode: input.mode,
            request: compiled.request,
            rules: compiled.rules,
            projections: compiled
                .unknowns
                .into_iter()
                .map(|unknown| ModelProjection {
                    variable: unknown.variable,
                    subject: unknown.node,
                    field: serde_json::to_value(unknown.field)
                        .ok()
                        .and_then(|value| value.as_str().map(ToOwned::to_owned))
                        .unwrap_or_else(|| "unknown".to_owned()),
                })
                .collect(),
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DesignToolRequest {
    #[serde(rename = "family")]
    _family: String,
    mode: RuleSolveMode,
    rules: Vec<DesignRuleBinding>,
    scene: DesignScene,
    #[serde(default)]
    unknowns: Vec<DesignUnknown>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    #[serde(default)]
    persist: bool,
    #[serde(default)]
    include_smt: bool,
}

const fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn solve_rules_schema() -> Value {
    let rule_ids = builtin_registry()
        .rules()
        .iter()
        .map(|rule| rule.id())
        .collect::<Vec<_>>();
    json!({
        "type": "object",
        "properties": {
            "family": {"const": "design"},
            "mode": {"type": "string", "enum": ["verify", "synthesize"]},
            "rules": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_DESIGN_RULE_BINDINGS,
                "items": {
                    "type": "object",
                    "properties": {
                        "rule_id": {"type": "string", "enum": rule_ids},
                        "subjects": {
                            "type": "array",
                            "minItems": 1,
                            "maxItems": MAX_DESIGN_NODES,
                            "items": {"type": "string"}
                        },
                        "parameters": design_rule_parameters_schema()
                    },
                    "required": ["rule_id", "subjects"],
                    "additionalProperties": false
                }
            },
            "scene": design_scene_schema(),
            "unknowns": {
                "type": "array",
                "maxItems": MAX_DESIGN_UNKNOWNS,
                "default": [],
                "items": {
                    "type": "object",
                    "properties": {
                        "node": {"type": "string"},
                        "field": {"type": "string", "enum": ["x", "y", "width", "height"]},
                        "min": {"type": "integer"},
                        "max": {"type": "integer"}
                    },
                    "required": ["node", "field", "min", "max"],
                    "additionalProperties": false
                }
            },
            "timeout_ms": {"type": "integer", "minimum": 1, "maximum": crate::types::MAX_TIMEOUT_MS, "default": DEFAULT_TIMEOUT_MS},
            "persist": {"type": "boolean", "default": false},
            "include_smt": {"type": "boolean", "default": false}
        },
        "required": ["family", "mode", "rules", "scene"],
        "additionalProperties": false
    })
}

fn design_rule_parameters_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "axis": {"type": "string", "enum": ["horizontal", "vertical"]},
            "gap": {"type": "integer", "minimum": 0},
            "inset_start": {"type": "integer", "minimum": 0},
            "inset_end": {"type": "integer", "minimum": 0},
            "padding": {"type": "integer", "minimum": 0},
            "minimum_gap": {"type": "integer", "minimum": 0},
            "source_width": {"type": "integer", "minimum": 1},
            "source_height": {"type": "integer", "minimum": 1}
        },
        "additionalProperties": false
    })
}

fn design_scene_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "viewport": {
                "type": "object",
                "properties": {
                    "width": {"type": "integer", "minimum": 1},
                    "height": {"type": "integer", "minimum": 1}
                },
                "required": ["width", "height"],
                "additionalProperties": false
            },
            "nodes": {
                "type": "object",
                "maxProperties": MAX_DESIGN_NODES,
                "additionalProperties": {
                    "type": "object",
                    "properties": {
                        "parent": {"type": "string"},
                        "rect": {
                            "type": "object",
                            "properties": {
                                "x": {"type": ["integer", "null"]},
                                "y": {"type": ["integer", "null"]},
                                "width": {"type": ["integer", "null"]},
                                "height": {"type": ["integer", "null"]}
                            },
                            "additionalProperties": false
                        }
                    },
                    "required": ["rect"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["viewport", "nodes"],
        "additionalProperties": false
    })
}

/// Returns the validated design catalog contribution.
#[must_use]
pub fn builtin_registry() -> &'static RuleRegistry {
    crate::rules::manifest::manifest_family_registry("design").expect("design manifest registry")
}
