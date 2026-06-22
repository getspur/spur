#![allow(dead_code)]

use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use spur_acp::{DelegationId, DelegationResult};
use tokio::sync::{oneshot, watch};

// ─── Request/Response types for orchestrator communication ────────────

/// Where a worker's worktree should be based, before any overlays.
///
/// Non-recursive sum type. Used as the inner `base` of `BaseSpec::WithOverlay`
/// to enforce that overlay chains cannot nest.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BaseTarget {
    /// Snapshot from the orchestrator's repo_root HEAD (legacy default).
    RepoMain,
    /// Branch by name.
    Branch { name: String },
    /// Pinned commit OID.
    Commit { oid: String },
}

// Tolerant deserializer: accepts the canonical object form AND a JSON-string
// form (e.g. `"{\"kind\":\"branch\",\"name\":\"x\"}"`). Some MCP clients
// (notably the Claude Code harness) JSON-stringify nested object arguments
// before transmitting them; without this adapter the server rejects such
// requests with `invalid type: string ..., expected internally tagged enum`.
impl<'de> serde::Deserialize<'de> for BaseTarget {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum Inner {
            RepoMain,
            Branch { name: String },
            Commit { oid: String },
        }
        let v = Value::deserialize(d)?;
        let inner: Inner = match v {
            Value::String(s) => serde_json::from_str(&s).map_err(serde::de::Error::custom)?,
            other => serde_json::from_value(other).map_err(serde::de::Error::custom)?,
        };
        Ok(match inner {
            Inner::RepoMain => BaseTarget::RepoMain,
            Inner::Branch { name } => BaseTarget::Branch { name },
            Inner::Commit { oid } => BaseTarget::Commit { oid },
        })
    }
}

/// Where a worker's worktree should be based.
///
/// Optional on `DelegateToWorkerInput` for backwards compatibility:
/// callers that omit `base` get the legacy behavior (snapshot from
/// repo_root HEAD, equivalent to `BaseSpec::RepoMain`).
///
/// Flat (non-recursive) by design: overlay chains nest at most one level
/// (overlay-on-base, never overlay-on-overlay). See spec Open Questions:
/// "Recommendation: flatten; nesting offers no operational benefit and
/// adds parsing complexity." This eliminates JSON-schema `$ref` recursion
/// that breaks MCP tool-calling in many LLM clients.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BaseSpec {
    /// Snapshot from the orchestrator's repo_root HEAD (legacy default).
    RepoMain,
    /// Branch by name.
    Branch { name: String },
    /// Pinned commit OID.
    Commit { oid: String },
    /// Apply cherry-pick overlays on top of a non-overlay base.
    WithOverlay {
        base: BaseTarget,
        overlays: Vec<OverlayCommit>,
    },
}

// Tolerant deserializer: see the matching impl on `BaseTarget` above for the
// rationale. The `Inner::WithOverlay.base` field is typed as `BaseTarget`, so
// recursion picks up `BaseTarget`'s tolerant impl automatically — a stringified
// `WithOverlay` whose nested `base` is also a string still parses correctly.
impl<'de> serde::Deserialize<'de> for BaseSpec {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum Inner {
            RepoMain,
            Branch {
                name: String,
            },
            Commit {
                oid: String,
            },
            WithOverlay {
                base: BaseTarget,
                overlays: Vec<OverlayCommit>,
            },
        }
        let v = Value::deserialize(d)?;
        let inner: Inner = match v {
            Value::String(s) => serde_json::from_str(&s).map_err(serde::de::Error::custom)?,
            other => serde_json::from_value(other).map_err(serde::de::Error::custom)?,
        };
        Ok(match inner {
            Inner::RepoMain => BaseSpec::RepoMain,
            Inner::Branch { name } => BaseSpec::Branch { name },
            Inner::Commit { oid } => BaseSpec::Commit { oid },
            Inner::WithOverlay { base, overlays } => BaseSpec::WithOverlay { base, overlays },
        })
    }
}

/// One overlay commit range to cherry-pick onto a base.
///
/// `base_oid..tip_oid` is the exclusive-of-base range.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
pub struct OverlayCommit {
    /// The plan task whose work this overlay represents (audit / signal context).
    pub source_task_id: String,
    /// Inclusive lower bound (exclusive of base in cherry-pick range).
    pub base_oid: String,
    /// Inclusive upper bound.
    pub tip_oid: String,
}

/// A delegation request sent from the MCP server to the orchestrator.
///
/// Each request carries a oneshot sender so the orchestrator can respond
/// directly to the originating handler — no shared response channel, no
/// ID-based matching, no dropped messages.
#[derive(Debug)]
pub struct DelegationRequest {
    pub id: DelegationId,
    pub agent: String,
    pub task: String,
    pub context_files: Vec<String>,
    /// Optional prior worker branch hint for future branch-reuse flows.
    /// In-process only (mpsc request path), not persisted/serialized.
    pub prior_branch_for_reuse: Option<String>,
    /// Oneshot channel for the orchestrator to send the result back.
    pub respond_to: oneshot::Sender<DelegationResult>,
    /// Brain session that originated this request. Threaded through so
    /// `DelegationRequested.from` / `DelegationDispatched.from` can
    /// correctly identify the brain in lineage. Stamped at every
    /// construction site in the MCP server.
    ///
    /// INV-2: typed as `BrainSessionId` (no `Default` impl) so callers
    /// cannot silently default to a phantom session id.
    pub brain_session_id: spur_acp::BrainSessionId,
    /// Structured reasoning trace the brain passed with this call.
    /// None when brain omitted the parameter. Orchestrator uses this
    /// for reviewer-visibility and mismatch detection. See design
    /// spec section C.
    pub delegation_plan: Option<spur_acp::DelegationPlan>,
    /// Optional beads issue ID to auto-track for this delegation.
    pub issue_id: Option<String>,
    /// Where to base the worker's worktree. None means legacy RepoMain behavior.
    ///
    /// Plan-engine dispatches will pass Some(WithOverlay { .. }) once bd-1dwm
    /// dispatch wiring lands; ad-hoc brain dispatches may omit it.
    pub base: Option<BaseSpec>,
    /// Publishes the worker worktree HEAD after overlay application back to the
    /// plan reconciler so completion audits can persist the true contribution
    /// base for the successful attempt. A watch sender is cloneable, so retry
    /// attempts can each publish their own resolved base and the reconciler can
    /// read the final value after the delegation result arrives.
    pub dispatched_base_oid_tx: Option<watch::Sender<Option<String>>>,
    /// Shared attempt tracker. Orchestrator updates this as review-loop
    /// retries advance so detached continuations can report the final
    /// 1-based worker attempt that produced the result.
    pub attempt_tracker: Arc<AtomicU32>,
    /// Default-on curated worker MCP subset. `None` (omitted) or
    /// `Some(true)` triggers the orchestrator's per-`BrainSession`
    /// `WorkerMcpServer` boot and a 1-hour HMAC token URL injection
    /// into the worker's `mcp_servers` config. Only `Some(false)`
    /// opts out and preserves the legacy "Workers get no MCP servers"
    /// behavior.
    pub enable_worker_mcp: Option<bool>,
}

/// Channel the orchestrator holds to receive requests from the MCP server.
pub struct DelegationChannel {
    pub request_rx: tokio::sync::mpsc::Receiver<DelegationRequest>,
}

#[cfg(test)]
mod base_spec_tests {
    use super::*;

    #[test]
    fn legacy_delegate_input_without_base_deserializes_as_none() {
        let json = serde_json::json!({
            "agent": "claude",
            "task": "do a thing",
        });
        let parsed: crate::tool_schemas::DelegateToWorkerInput =
            serde_json::from_value(json).expect("legacy input must parse");
        assert!(parsed.base.is_none(), "missing base must default to None");
    }

    #[test]
    fn delegate_input_with_base_repo_main_deserializes() {
        let json = serde_json::json!({
            "agent": "claude",
            "task": "do a thing",
            "base": { "kind": "repo_main" },
        });
        let parsed: crate::tool_schemas::DelegateToWorkerInput =
            serde_json::from_value(json).unwrap();
        assert!(matches!(parsed.base, Some(BaseSpec::RepoMain)));
    }

    #[test]
    fn delegate_input_with_overlay_deserializes() {
        let json = serde_json::json!({
            "agent": "claude",
            "task": "do a thing",
            "base": {
                "kind": "with_overlay",
                "base": { "kind": "branch", "name": "spur/plan-base-xyz" },
                "overlays": [
                    { "source_task_id": "T1", "base_oid": "aaa", "tip_oid": "bbb" }
                ]
            }
        });
        let parsed: crate::tool_schemas::DelegateToWorkerInput =
            serde_json::from_value(json).unwrap();
        match &parsed.base {
            Some(BaseSpec::WithOverlay { base, overlays }) => {
                assert!(
                    matches!(base, BaseTarget::Branch { name } if name == "spur/plan-base-xyz")
                );
                assert_eq!(overlays.len(), 1);
                assert_eq!(overlays[0].source_task_id, "T1");
            }
            other => panic!("expected WithOverlay, got {other:?}"),
        }
    }

    // Tolerant-string-form coverage: some MCP clients JSON-stringify nested
    // object arguments before transmission. The custom Deserialize impls on
    // BaseSpec / BaseTarget must accept that shape too.

    #[test]
    fn basespec_string_form_repo_main() {
        let v = Value::String(r#"{"kind":"repo_main"}"#.into());
        let parsed: BaseSpec = serde_json::from_value(v).unwrap();
        assert_eq!(parsed, BaseSpec::RepoMain);
    }

    #[test]
    fn basespec_string_form_branch() {
        let v = Value::String(r#"{"kind":"branch","name":"feat/foo"}"#.into());
        let parsed: BaseSpec = serde_json::from_value(v).unwrap();
        assert_eq!(
            parsed,
            BaseSpec::Branch {
                name: "feat/foo".into()
            }
        );
    }

    #[test]
    fn basespec_string_form_commit() {
        let oid = "0123456789012345678901234567890123456789";
        let v = Value::String(format!(r#"{{"kind":"commit","oid":"{oid}"}}"#));
        let parsed: BaseSpec = serde_json::from_value(v).unwrap();
        assert_eq!(parsed, BaseSpec::Commit { oid: oid.into() });
    }

    #[test]
    fn basespec_string_form_with_overlay() {
        let v = Value::String(
            r#"{"kind":"with_overlay","base":{"kind":"branch","name":"x"},"overlays":[]}"#.into(),
        );
        let parsed: BaseSpec = serde_json::from_value(v).unwrap();
        match parsed {
            BaseSpec::WithOverlay { base, overlays } => {
                assert_eq!(base, BaseTarget::Branch { name: "x".into() });
                assert!(overlays.is_empty());
            }
            other => panic!("expected WithOverlay, got {other:?}"),
        }
    }

    #[test]
    fn basespec_string_form_with_overlay_nested_string() {
        // Even the inner BaseTarget arrives stringified — recursion through
        // the BaseTarget tolerant impl must still parse it.
        let v = Value::String(
            r#"{"kind":"with_overlay","base":"{\"kind\":\"branch\",\"name\":\"x\"}","overlays":[]}"#
                .into(),
        );
        let parsed: BaseSpec = serde_json::from_value(v).unwrap();
        match parsed {
            BaseSpec::WithOverlay { base, overlays } => {
                assert_eq!(base, BaseTarget::Branch { name: "x".into() });
                assert!(overlays.is_empty());
            }
            other => panic!("expected WithOverlay, got {other:?}"),
        }
    }

    #[test]
    fn basespec_malformed_string_errors() {
        let v = Value::String("this is not json".into());
        let err = serde_json::from_value::<BaseSpec>(v).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("expected") || msg.contains("invalid"),
            "expected serde parse error, got: {msg}",
        );
    }

    #[test]
    fn basespec_string_form_unknown_kind_errors() {
        let v = Value::String(r#"{"kind":"unknown_thing"}"#.into());
        assert!(serde_json::from_value::<BaseSpec>(v).is_err());
    }

    #[test]
    fn basetarget_string_form_branch() {
        let v = Value::String(r#"{"kind":"branch","name":"feat/foo"}"#.into());
        let parsed: BaseTarget = serde_json::from_value(v).unwrap();
        assert_eq!(
            parsed,
            BaseTarget::Branch {
                name: "feat/foo".into()
            }
        );
    }

    #[test]
    fn basetarget_string_form_repo_main() {
        let v = Value::String(r#"{"kind":"repo_main"}"#.into());
        let parsed: BaseTarget = serde_json::from_value(v).unwrap();
        assert_eq!(parsed, BaseTarget::RepoMain);
    }

    #[test]
    fn basetarget_string_form_commit() {
        let oid = "0123456789012345678901234567890123456789";
        let v = Value::String(format!(r#"{{"kind":"commit","oid":"{oid}"}}"#));
        let parsed: BaseTarget = serde_json::from_value(v).unwrap();
        assert_eq!(parsed, BaseTarget::Commit { oid: oid.into() });
    }
}

// ─── Tool definition ──────────────────────────────────────────────────

/// Metadata for a single MCP tool, returned by `tools/list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
}

fn pm_tool_definition(definition: spur_pm::mcp::ToolDefinition) -> ToolDefinition {
    ToolDefinition {
        name: definition.name,
        description: definition.description,
        input_schema: definition.input_schema,
    }
}

fn pm_tool_definitions_by_names(names: &[&str]) -> Vec<ToolDefinition> {
    let definitions = spur_pm::mcp::tool_definitions();
    names
        .iter()
        .map(|name| {
            definitions
                .iter()
                .find(|definition| definition.name == *name)
                .unwrap_or_else(|| panic!("spur-pm MCP module missing tool definition {name}"))
                .clone()
        })
        .map(pm_tool_definition)
        .collect()
}

pub(crate) fn legacy_prelude_tool_definitions() -> Vec<ToolDefinition> {
    Vec::new()
}

pub(crate) fn legacy_plan_management_tool_definitions() -> Vec<ToolDefinition> {
    Vec::new()
}

pub(crate) fn legacy_remainder_tool_definitions() -> Vec<ToolDefinition> {
    Vec::new()
}

/// Returns the legacy `spur-mcp` brain tool definitions.
///
/// Application crates compose externally owned modules, such as
/// `spur_core::mcp::delegation` and `spur_core::mcp::signals`, into the
/// per-server registry.
pub fn tools_list() -> Vec<ToolDefinition> {
    crate::registry::default_tool_registry()
        .expect("default MCP tool registry must be valid")
        .list_tools()
}

/// Curated worker-facing tool subset exposed by `WorkerMcpServer`.
///
/// Workers receive only read-and-emit tools: read tools that surface
/// project state plus 2 fire-and-forget signal tools (`report_signal`,
/// `report_progress`). Brain-only orchestration
/// tools (delegate_*, submit_plan, merge_plan, execute_epic, review_task,
/// update_issue, create_pr, create_issue, add_dependency, cancel_delegation,
/// list_available_workers, get_reconciler_status, graph_*, preview_task_base,
/// plan_truncate_and_restart) are intentionally excluded — exposing them to
/// workers would invert the brain→worker authority direction and let workers
/// self-dispatch.
pub(crate) fn legacy_worker_prelude_tool_definitions() -> Vec<ToolDefinition> {
    pm_tool_definitions_by_names(&["get_issue", "list_issues"])
}

pub(crate) fn legacy_worker_remainder_tool_definitions() -> Vec<ToolDefinition> {
    Vec::new()
}

pub fn worker_tools_list() -> Vec<ToolDefinition> {
    crate::registry::default_worker_tool_registry()
        .expect("default worker MCP tool registry must be valid")
        .list_tools()
}

#[cfg(test)]
mod schema_truthfulness_tests {
    use super::*;
    use serde_json::json;
    use std::sync::OnceLock;
    use tokio::sync::Mutex as TokioMutex;

    static CWD_LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();

    async fn cwd_lock() -> tokio::sync::MutexGuard<'static, ()> {
        CWD_LOCK.get_or_init(|| TokioMutex::new(())).lock().await
    }

    struct CwdGuard {
        original: std::path::PathBuf,
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }

    fn enter_dir(path: &std::path::Path) -> CwdGuard {
        let original = std::env::current_dir().expect("current dir");
        std::env::set_current_dir(path).expect("set current dir");
        CwdGuard { original }
    }

    fn props_of(def: &ToolDefinition) -> Vec<String> {
        def.input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    }

    fn analyst_tool_def(name: &str) -> ToolDefinition {
        let definition = spur_analyst::mcp::tool_definitions()
            .into_iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("spur-analyst MCP module missing tool definition {name}"));
        ToolDefinition {
            name: definition.name,
            description: definition.description,
            input_schema: definition.input_schema,
        }
    }

    #[test]
    fn get_issue_schema_does_not_advertise_source() {
        let def = pm_tool_definitions_by_names(&["get_issue"])
            .into_iter()
            .next()
            .expect("get_issue definition");
        assert!(
            !props_of(&def).contains(&"source".to_string()),
            "get_issue must not advertise `source` until multi-backend lands",
        );
    }

    #[test]
    fn update_issue_schema_does_not_advertise_source() {
        let def = pm_tool_definitions_by_names(&["update_issue"])
            .into_iter()
            .next()
            .expect("update_issue definition");
        assert!(
            !props_of(&def).contains(&"source".to_string()),
            "update_issue must not advertise `source` until multi-backend lands",
        );
    }

    #[test]
    fn delegation_tools_are_not_owned_by_legacy_tools_list() {
        let tools = tools_list();
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        for tool in [
            "delegate_to_worker",
            "delegate_parallel",
            "check_delegation_status",
            "fetch_outcome_artifact",
            "cancel_delegation",
            "list_available_workers",
        ] {
            assert!(
                !names.contains(&tool),
                "{tool} must be owned by spur_core::mcp::delegation, got: {names:?}"
            );
        }
    }

    #[test]
    fn plan_tools_are_not_owned_by_legacy_tools_list() {
        let mut tools = legacy_plan_management_tool_definitions();
        tools.extend(legacy_remainder_tool_definitions());
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        for tool in [
            "merge_plan",
            "resume_plan",
            "force_reclaim_plan",
            "submit_plan",
            "execute_epic",
            "get_plan_status",
            "get_reconciler_status",
            "get_task_diff",
            "preview_task_base",
            "plan_truncate_and_restart",
            "recover_orphaned_dispatch",
            "review_task",
            "submit_plan_mutation",
        ] {
            assert!(
                !names.contains(&tool),
                "{tool} must be owned by spur_core::mcp::plan, got: {names:?}"
            );
        }
    }

    #[test]
    fn knowledge_context_pack_schema_matches_contract() {
        let def = analyst_tool_def("knowledge_context_pack");
        let props = def
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("properties");
        let stale_ann_boundary = ["Lance ANN is", "not used by this MVP"].join(" ");

        assert!(
            def.description.contains("Deprecated alias")
                && def.description.contains("knowledge_context_pack_2")
                && def.description.contains("v2 behavior")
                && !def.description.contains(&stale_ann_boundary),
            "knowledge_context_pack description must mark v1 as a deprecated alias to v2"
        );
        assert_eq!(def.input_schema.get("required"), Some(&json!(["query"])));
        assert_eq!(
            def.input_schema.get("additionalProperties"),
            Some(&json!(false))
        );
        let mut prop_names = props.keys().cloned().collect::<Vec<_>>();
        prop_names.sort();
        assert_eq!(
            prop_names,
            vec![
                "graph_reasoning",
                "include_tests",
                "intent",
                "limit",
                "max_symbol_bodies",
                "query",
                "scope",
            ],
            "knowledge_context_pack alias must advertise the v2 input shape",
        );
        assert_eq!(
            props.get("query").and_then(|v| v.get("minLength")),
            Some(&json!(1))
        );
        assert_eq!(
            props.get("intent").and_then(|v| v.get("enum")),
            Some(&json!(["explain", "change", "review", "debug", "plan"]))
        );
        assert_eq!(
            props.get("intent").and_then(|v| v.get("default")),
            Some(&json!("explain"))
        );
        assert_eq!(
            props.get("scope").and_then(|v| v.get("enum")),
            Some(&json!(["all", "docs", "code", "graph"]))
        );
        assert_eq!(
            props.get("scope").and_then(|v| v.get("default")),
            Some(&json!("all"))
        );
        assert_eq!(
            props.get("limit").and_then(|v| v.get("minimum")),
            Some(&json!(1))
        );
        assert_eq!(
            props.get("limit").and_then(|v| v.get("maximum")),
            Some(&json!(20))
        );
        assert_eq!(
            props.get("limit").and_then(|v| v.get("default")),
            Some(&json!(8))
        );
        assert_eq!(
            props.get("include_tests").and_then(|v| v.get("default")),
            Some(&json!(true))
        );
        assert_eq!(
            props
                .get("max_symbol_bodies")
                .and_then(|v| v.get("minimum")),
            Some(&json!(0))
        );
        assert_eq!(
            props
                .get("max_symbol_bodies")
                .and_then(|v| v.get("maximum")),
            Some(&json!(5))
        );
        assert_eq!(
            props
                .get("max_symbol_bodies")
                .and_then(|v| v.get("default")),
            Some(&json!(3))
        );
    }

    #[test]
    fn knowledge_context_pack_2_schema_matches_contract() {
        let def = analyst_tool_def("knowledge_context_pack_2");
        let props = def
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("properties");

        assert!(
            def.description.contains("First-class")
                && def.description.contains("canonical")
                && !def.description.contains("experimental")
                && def.description.contains("structured evidence pack")
                && def.description.contains("semantic answers")
                && def.description.contains("does not generate final prose")
                && def.description.contains("DuckPGQ/Onager")
                && def.description.contains("graph_paths")
                && def.description.contains("risk_scorecard")
                && def.description.contains("community_context"),
            "knowledge_context_pack_2 description must advertise graph reasoning sections"
        );
        assert_eq!(def.input_schema.get("required"), Some(&json!(["query"])));
        assert_eq!(
            def.input_schema.get("additionalProperties"),
            Some(&json!(false))
        );
        let mut prop_names = props.keys().cloned().collect::<Vec<_>>();
        prop_names.sort();
        assert_eq!(
            prop_names,
            vec![
                "graph_reasoning",
                "include_tests",
                "intent",
                "limit",
                "max_symbol_bodies",
                "query",
                "scope",
            ],
            "knowledge_context_pack_2 property set drifted",
        );

        let graph_reasoning = props
            .get("graph_reasoning")
            .and_then(|v| v.get("properties"))
            .and_then(|v| v.as_object())
            .expect("graph_reasoning properties");
        let mut graph_prop_names = graph_reasoning.keys().cloned().collect::<Vec<_>>();
        graph_prop_names.sort();
        assert_eq!(
            graph_prop_names,
            vec![
                "anchors",
                "communities",
                "max_path_hops",
                "max_paths",
                "paths",
                "risk",
            ],
            "knowledge_context_pack_2 graph_reasoning property set drifted",
        );
        assert_eq!(
            graph_reasoning
                .get("max_path_hops")
                .and_then(|v| v.get("minimum")),
            Some(&json!(1))
        );
        assert_eq!(
            graph_reasoning
                .get("max_path_hops")
                .and_then(|v| v.get("maximum")),
            Some(&json!(6))
        );
        assert_eq!(
            graph_reasoning
                .get("max_paths")
                .and_then(|v| v.get("minimum")),
            Some(&json!(1))
        );
        assert_eq!(
            graph_reasoning
                .get("max_paths")
                .and_then(|v| v.get("maximum")),
            Some(&json!(12))
        );
        assert_eq!(
            graph_reasoning
                .get("anchors")
                .and_then(|v| v.get("items"))
                .and_then(|v| v.get("type")),
            Some(&json!("string"))
        );
    }

    #[tokio::test]
    async fn code_graph_schemas_advertise_selector_legacy_symbol_and_ambiguity_mode() {
        let graph_defs = spur_graph::mcp::tool_definitions();
        for name in [
            "code_symbol_info",
            "code_callers",
            "code_callees",
            "code_subgraph",
        ] {
            let def = graph_defs
                .iter()
                .find(|definition| definition.name == name)
                .unwrap_or_else(|| panic!("missing graph tool definition {name}"));
            let props = def
                .input_schema
                .get("properties")
                .and_then(|v| v.as_object())
                .unwrap_or_else(|| panic!("{} properties", def.name));
            assert!(props.contains_key("selector"), "{} selector", def.name);
            assert!(props.contains_key("symbol"), "{} legacy symbol", def.name);
            assert!(
                props
                    .get("symbol")
                    .and_then(|v| v.get("description"))
                    .and_then(|v| v.as_str())
                    .is_some_and(|description| description
                        == "deprecated; use selector. Accepts graph://symbol/<id> or bare hex id."),
                "{} symbol deprecation description",
                def.name
            );
            assert!(
                def.input_schema.get("anyOf").is_none(),
                "{} must not advertise top-level anyOf",
                def.name
            );
            if def.name == "code_symbol_info" {
                continue;
            }
            assert_eq!(
                props.get("on_ambiguous").and_then(|v| v.get("enum")),
                Some(&json!(["candidates", "error"])),
                "{} on_ambiguous enum",
                def.name
            );
        }

        let _lock = cwd_lock().await;
        let dir = tempfile::TempDir::new().expect("tempdir");
        std::fs::create_dir_all(dir.path().join(".spur")).expect("create .spur");
        std::fs::write(
            dir.path().join(".spur/graph-index.json"),
            serde_json::to_string_pretty(&json!({
                "header": { "graph_index_version": "test" },
                "manifest_version": "test",
                "graph_content_hash": "test",
                "files": [],
                "symbols": [],
                "edges": [],
                "tombstones": []
            }))
            .expect("encode graph fixture"),
        )
        .expect("write graph fixture");
        let _cwd = enter_dir(dir.path());

        let error = spur_graph::mcp::code_callers(&json!({}))
            .await
            .expect_err("handler must reject calls without selector or symbol");
        assert!(matches!(
            error,
            spur_graph::mcp::McpHandlerError::InvalidParams(_)
        ));
        assert!(
            error
                .to_string()
                .contains("Missing required field 'selector' (or deprecated 'symbol')"),
            "unexpected validation error: {error}"
        );
    }

    #[test]
    fn code_search_schema_uses_query_not_selector_or_legacy_symbol() {
        let graph_defs = spur_graph::mcp::tool_definitions();
        let def = graph_defs
            .iter()
            .find(|definition| definition.name == "code_symbol_search")
            .expect("code_symbol_search graph tool definition");
        let props = def
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("properties");

        assert!(props.contains_key("query"), "code_search query");
        assert!(props.contains_key("mode"), "code_search mode");
        assert!(props.contains_key("symbol_kind"), "code_search symbol_kind");
        assert!(props.contains_key("file"), "code_search file");
        assert!(props.contains_key("file_glob"), "code_search file_glob");
        assert!(props.contains_key("limit"), "code_search limit");
        assert!(
            !props.contains_key("selector"),
            "code_search must not advertise selector"
        );
        assert!(
            !props.contains_key("symbol"),
            "code_search must not advertise legacy symbol"
        );
        assert!(
            !props.contains_key("on_ambiguous"),
            "code_search must not advertise graph-resolution ambiguity controls"
        );
        assert_eq!(
            props.get("query").and_then(|v| v.get("minLength")),
            Some(&json!(1))
        );
        assert_eq!(
            props.get("mode").and_then(|v| v.get("enum")),
            Some(&json!(["exact", "prefix", "substring"]))
        );
        assert_eq!(
            props.get("mode").and_then(|v| v.get("default")),
            Some(&json!("substring"))
        );
        assert_eq!(
            props.get("limit").and_then(|v| v.get("minimum")),
            Some(&json!(1))
        );
        assert_eq!(
            props.get("limit").and_then(|v| v.get("maximum")),
            Some(&json!(200))
        );
        assert_eq!(
            props.get("limit").and_then(|v| v.get("default")),
            Some(&json!(20))
        );
        assert_eq!(def.input_schema.get("required"), Some(&json!(["query"])));
    }
}

#[cfg(test)]
mod worker_tools_subset_tests {
    use super::*;

    #[test]
    fn tools_list_contains_exactly_the_compatibility_set() {
        let actual: Vec<String> = tools_list().iter().map(|t| t.name.clone()).collect();
        assert!(
            actual.is_empty(),
            "spur-mcp no longer owns the brain tool catalog after core extraction; got {actual:?}",
        );
    }

    #[test]
    fn default_tool_registry_preserves_code_search_alias_without_advertising_it() {
        let registry = crate::registry::default_tool_registry().expect("default registry");
        let names: Vec<String> = registry
            .list_tools()
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        assert!(!names.contains(&"code_search".to_string()));
        assert_eq!(registry.canonical_name("code_search"), None);
    }

    #[test]
    fn knowledge_context_pack_appears_in_worker_tools_list() {
        let actual: Vec<String> = spur_analyst::mcp::tool_definitions()
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        assert!(actual.contains(&"knowledge_context_pack".to_string()));
        assert!(actual.contains(&"knowledge_context_pack_2".to_string()));
    }

    #[test]
    fn analyst_tools_are_not_owned_by_legacy_remainder() {
        let actual: Vec<String> = legacy_remainder_tool_definitions()
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        for name in [
            "doc_navigate",
            "knowledge_context_pack",
            "knowledge_context_pack_2",
        ] {
            assert!(
                !actual.contains(&name.to_string()),
                "{name} must be owned by spur_analyst::mcp, not the legacy remainder"
            );
        }
    }

    #[test]
    fn analyst_worker_tools_are_not_owned_by_legacy_worker_remainder() {
        let actual: Vec<String> = legacy_worker_remainder_tool_definitions()
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        for name in [
            "doc_navigate",
            "knowledge_context_pack",
            "knowledge_context_pack_2",
        ] {
            assert!(
                !actual.contains(&name.to_string()),
                "{name} must be owned by spur_analyst::mcp, not the legacy worker remainder"
            );
        }
    }

    #[test]
    fn worker_signal_tools_are_not_owned_by_legacy_remainder() {
        let actual: Vec<String> = legacy_remainder_tool_definitions()
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        for name in ["report_signal", "report_progress"] {
            assert!(
                !actual.contains(&name.to_string()),
                "{name} must be owned by spur_core::mcp::signals, not the legacy remainder"
            );
        }
    }

    #[test]
    fn worker_signal_tools_are_not_owned_by_legacy_worker_remainder() {
        let actual: Vec<String> = legacy_worker_remainder_tool_definitions()
            .iter()
            .map(|tool| tool.name.clone())
            .collect();
        for name in ["report_signal", "report_progress"] {
            assert!(
                !actual.contains(&name.to_string()),
                "{name} must be owned by spur_core::mcp::signals, not the legacy worker remainder"
            );
        }
    }

    #[test]
    fn worker_tools_list_contains_exactly_the_curated_set() {
        let actual: Vec<String> = worker_tools_list().iter().map(|t| t.name.clone()).collect();
        assert!(
            actual.is_empty(),
            "spur-mcp no longer owns the worker tool catalog after core extraction; got {actual:?}",
        );
    }

    #[test]
    fn worker_tools_list_excludes_brain_only_tools() {
        let actual: Vec<String> = worker_tools_list().iter().map(|t| t.name.clone()).collect();
        let forbidden = [
            "delegate_to_worker",
            "delegate_parallel",
            "check_delegation_status",
            "cancel_delegation",
            "list_available_workers",
            "create_issue",
            "add_dependency",
            "create_pr",
            "merge_plan",
            "submit_plan",
            "submit_plan_mutation",
            "execute_epic",
            "get_reconciler_status",
            "preview_task_base",
            "plan_truncate_and_restart",
            "recover_orphaned_dispatch",
            "review_task",
            "graph_triage",
            "graph_plan",
            "graph_insights",
            "graph_alerts",
            "graph_subgraph",
        ];
        assert!(
            !forbidden.contains(&"code_symbol_search"),
            "code_symbol_search is read-only and worker-facing, not brain-only",
        );
        for tool in &forbidden {
            assert!(
                !actual.iter().any(|n| n == tool),
                "leaked brain-only tool into worker subset: {tool}",
            );
        }
    }

    #[test]
    fn worker_tools_list_excludes_pm_write_tools() {
        let actual: Vec<String> = worker_tools_list().iter().map(|t| t.name.clone()).collect();
        for tool in [
            "update_issue",
            "create_issue",
            "add_dependency",
            "create_pr",
        ] {
            assert!(
                !actual.iter().any(|n| n == tool),
                "leaked PM write tool into worker subset: {tool}",
            );
        }
    }

    #[test]
    fn worker_tools_list_is_a_strict_subset_of_tools_list() {
        let full: std::collections::HashSet<String> =
            tools_list().iter().map(|t| t.name.clone()).collect();
        let externally_owned_worker_tools = std::collections::HashSet::from([
            "report_signal".to_string(),
            "report_progress".to_string(),
            "fetch_outcome_artifact".to_string(),
        ]);
        for w in worker_tools_list() {
            assert!(
                full.contains(&w.name) || externally_owned_worker_tools.contains(&w.name),
                "worker tool '{}' missing from full tools_list or externally owned worker tool set",
                w.name,
            );
        }
    }
}

#[cfg(test)]
mod tool_registry_tests {
    use super::*;
    use crate::registry::{ToolCallContext, ToolModule, ToolRegistry, ToolResponse};
    use async_trait::async_trait;
    use rmcp::model::ErrorData as McpError;
    use serde_json::json;

    struct StaticToolModule {
        name: &'static str,
    }

    #[async_trait]
    impl ToolModule for StaticToolModule {
        fn tools(&self) -> Vec<ToolDefinition> {
            vec![ToolDefinition {
                name: self.name.to_string(),
                description: "test tool".to_string(),
                input_schema: json!({ "type": "object" }),
            }]
        }

        async fn call(
            &self,
            _ctx: ToolCallContext<'_>,
            _name: &str,
            _args: Value,
        ) -> Result<ToolResponse, McpError> {
            unreachable!("registry duplicate test never invokes tools")
        }
    }

    #[test]
    fn tool_registry_rejects_duplicate_tool_names() {
        let mut registry = ToolRegistry::new();
        registry
            .register(StaticToolModule { name: "duplicate" })
            .expect("first registration succeeds");

        let err = registry
            .register(StaticToolModule { name: "duplicate" })
            .expect_err("duplicate tool names must be rejected");

        assert!(
            err.to_string().contains("duplicate"),
            "unexpected duplicate error: {err}"
        );
    }
}

#[cfg(test)]
mod base_target_round_trip {
    use super::BaseTarget;

    #[test]
    fn repo_main_round_trips() {
        let v = serde_json::to_value(BaseTarget::RepoMain).unwrap();
        let back: BaseTarget = serde_json::from_value(v).unwrap();
        assert_eq!(back, BaseTarget::RepoMain);
    }

    #[test]
    fn branch_round_trips() {
        let v = serde_json::to_value(BaseTarget::Branch {
            name: "feature/x".into(),
        })
        .unwrap();
        let back: BaseTarget = serde_json::from_value(v).unwrap();
        assert_eq!(
            back,
            BaseTarget::Branch {
                name: "feature/x".into()
            }
        );
    }

    #[test]
    fn commit_round_trips() {
        let v = serde_json::to_value(BaseTarget::Commit {
            oid: "abc123".into(),
        })
        .unwrap();
        let back: BaseTarget = serde_json::from_value(v).unwrap();
        assert_eq!(
            back,
            BaseTarget::Commit {
                oid: "abc123".into()
            }
        );
    }
}
