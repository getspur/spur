use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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
        match parsed.base {
            Some(BaseSpec::WithOverlay {
                ref base,
                ref overlays,
            }) => {
                assert!(
                    matches!(base, BaseTarget::Branch { name } if name == "spur/plan-base-xyz")
                );
                assert_eq!(overlays.len(), 1);
                assert_eq!(overlays[0].source_task_id, "T1");
            }
            _ => panic!("expected WithOverlay, got {:?}", parsed.base),
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
        let v = Value::String(format!(r#"{{"kind":"commit","oid":"{}"}}"#, oid));
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
            other => panic!("expected WithOverlay, got {:?}", other),
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
            other => panic!("expected WithOverlay, got {:?}", other),
        }
    }

    #[test]
    fn basespec_malformed_string_errors() {
        let v = Value::String("this is not json".into());
        let err = serde_json::from_value::<BaseSpec>(v).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("expected") || msg.contains("invalid"),
            "expected serde parse error, got: {}",
            msg
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
        let v = Value::String(format!(r#"{{"kind":"commit","oid":"{}"}}"#, oid));
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

// ─── Tool definitions ─────────────────────────────────────────────────

fn delegate_to_worker_def() -> ToolDefinition {
    ToolDefinition {
        name: "delegate_to_worker".into(),
        description: "Delegate a task to a worker agent. Returns inline if the worker finishes within the inline-wait window (configurable via `delegation.inline_wait_ms`; default 0). Otherwise returns `{status: \"pending\", delegation_id}` and you will be re-prompted automatically when the worker completes — you do not need to poll. Pass a `delegation_plan` parameter (at minimum `{chosen, rationale}`; more for multi-step work). Structure the `task` field as CONTEXT / GOAL / CONSTRAINTS / EXPECTED_OUTPUT. `enable_worker_mcp` defaults to on — the worker receives the curated worker MCP server unless you pass `false`. `enable_worker_progress` defaults to off; opt in for progress events. Use `list_available_workers` when routing is ambiguous.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::DelegateToWorkerInput>(),
    }
}

fn delegate_parallel_def() -> ToolDefinition {
    ToolDefinition {
        name: "delegate_parallel".into(),
        description: "Delegate multiple tasks in parallel. Returns a response array of length N; each element is either an inline result or `{status: \"pending\", delegation_id}` with an automatic re-prompt when that task completes. Each task's per-task `delegation_plan` documents structured reasoning for reviewer mismatch checks. Per-task `enable_worker_mcp` defaults to on — each worker receives the curated worker MCP server unless explicitly set to `false`. `enable_worker_progress` defaults to off; opt in per task for progress events. Subtasks MUST be independent — no shared state, no sequential data dependencies. If unsure, use `delegate_to_worker` serially.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::DelegateParallelInput>(),
    }
}

fn list_available_workers_def() -> ToolDefinition {
    ToolDefinition {
        name: "list_available_workers".into(),
        description: "Returns tier, description, good_for, avoid_for, output_shape, and cost_tier for each worker. Call when the system-prompt one-liner is insufficient.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
    }
}

fn merge_plan_def() -> ToolDefinition {
    ToolDefinition {
        name: "merge_plan".into(),
        description: "Integrate a fully approved plan onto a dedicated plan-scoped branch. Cherry-picks approved worker branches in deterministic topological order without mutating the active checkout. On success, returns a `merge_branch` you can pass to `create_pr`. On conflict, returns the partial branch plus the conflicting task and files.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_id": {
                    "type": "string",
                    "description": "The plan_id returned by submit_plan or execute_epic"
                }
            },
            "required": ["plan_id"]
        }),
    }
}

fn resume_plan_def() -> ToolDefinition {
    ToolDefinition {
        name: "resume_plan".into(),
        description: "Explicitly claim or resume a persisted beads plan. MVP claims unowned plans and refuses plans with active owners; active handoff is not implemented.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_id": {
                    "type": "string",
                    "description": "The persisted beads plan_id to claim or resume"
                }
            },
            "required": ["plan_id"]
        }),
    }
}

fn force_reclaim_plan_def() -> ToolDefinition {
    ToolDefinition {
        name: "force_reclaim_plan".into(),
        description: "Operator-initiated force-takeover of plan ownership. Removes any existing `spur:plan-owner:*` labels and stamps the current brain as the owner. Intended only for stuck/dead owners or governance-driven takeover; clobbers any concurrent owner brain's in-flight state. Requires explicit `confirm: true`. See docs/multi-brain-operations.md.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_id": {
                    "type": "string",
                    "description": "The plan_id whose ownership to force-reclaim"
                },
                "confirm": {
                    "type": "boolean",
                    "description": "Must be `true` to acknowledge that this clobbers any concurrent owner brain's in-flight state. `false` or missing returns an error."
                },
                "reason": {
                    "type": "string",
                    "description": "Optional human-readable reason recorded in the audit sentinel for accountability"
                }
            },
            "required": ["plan_id", "confirm"]
        }),
    }
}

fn check_delegation_status_def() -> ToolDefinition {
    ToolDefinition {
        name: "check_delegation_status".into(),
        description: "Non-blocking status query for a delegation. Returns the result if finished, or `{\"status\":\"running\"}`. Primarily a debugging affordance — brains are re-prompted automatically when delegations complete and normally do not need to call this.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "delegation_id": {
                    "type": "string",
                    "description": "The delegation_id to check"
                }
            },
            "required": ["delegation_id"]
        }),
    }
}

fn fetch_outcome_artifact_def() -> ToolDefinition {
    ToolDefinition {
        name: "fetch_outcome_artifact".into(),
        description: "Fetch the side-channel artifact (full or sectioned) for a completed delegation. Use when continuation.payload.artifact_id is Some(_) and you need fuller context. Sections let you pick what to fetch: pass 'status_only' for just status fields (~100B), 'summary' for the inline summary, 'diff_only' for full diff text, or 'full' for the entire DelegationResult JSON.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "delegation_id": {
                    "type": "string",
                    "description": "The delegation_id whose artifact you want to fetch."
                },
                "attempt": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional attempt number. Default: latest known attempt for this delegation. Pin a specific attempt for forensic queries on retried delegations."
                },
                "section": {
                    "type": "string",
                    "enum": ["status_only", "summary", "diff_only", "full"],
                    "default": "full",
                    "description": "Which section to fetch."
                }
            },
            "required": ["delegation_id"]
        }),
    }
}

fn cancel_delegation_def() -> ToolDefinition {
    ToolDefinition {
        name: "cancel_delegation".into(),
        description: "Request cancellation of a running delegation. If the delegation already completed, returns its result. Otherwise forwards the cancellation to the orchestrator and returns its response.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "delegation_id": {
                    "type": "string",
                    "description": "The delegation_id to cancel"
                }
            },
            "required": ["delegation_id"]
        }),
    }
}

// ─── Plan execution tool definitions ──────────────────────────────

fn submit_plan_def() -> ToolDefinition {
    ToolDefinition {
        name: "submit_plan".into(),
        description: "Submit a structured execution plan with dependency ordering. The orchestrator dispatches tasks to workers automatically: independent tasks run in parallel, dependent tasks wait for predecessors to complete. Use graph_plan to get dependency-aware tracks, then enrich each item with agent assignment and task description. Returns a plan_id — poll with get_plan_status.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "task_id": {
                                "type": "string",
                                "description": "Unique identifier for this task (use issue ID or descriptive slug)"
                            },
                            "agent": {
                                "type": "string",
                                "description": "Worker agent to execute this task"
                            },
                            "task": {
                                "type": "string",
                                "description": "Task description (CONTEXT / GOAL / CONSTRAINTS / EXPECTED_OUTPUT)"
                            },
                            "depends_on": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "task_ids that must complete before this task starts. Empty or omitted = ready immediately."
                            },
                            "issue_id": {
                                "type": "string",
                                "description": "Optional beads issue ID to auto-track"
                            },
                            "context_files": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Optional file paths for worker context"
                            }
                        },
                        "required": ["task_id", "agent", "task"]
                    },
                    "description": "Tasks with dependency edges forming a DAG. Tasks with no depends_on are dispatched immediately."
                },
                "epic_title": {
                    "type": "string",
                    "description": "Epic title. Required when the first task description is empty or whitespace-only."
                },
                "epic_body": {
                    "type": "string",
                    "description": "Epic description / rationale. Optional."
                },
                "client_idempotency_key": {
                    "type": "string",
                    "description": "Optional caller-supplied idempotency key. When repeated with a beads-backed persisted submit_plan within the dedup TTL, returns the existing plan_id without creating another epic."
                },
                "base": {
                    "description": "Optional explicit base for the plan. Omit (or pass {\"kind\":\"repo_main\"}) for legacy behavior — the plan engine snapshots the brain working tree HEAD. Pass {\"kind\":\"branch\",\"name\":\"<branch>\"} or {\"kind\":\"commit\",\"oid\":\"<oid>\"} to base the plan on a named ref instead; the brain working tree is not touched. Useful for stacking plans on a prior phase's integration branch.",
                    "oneOf": [
                        {
                            "type": "object",
                            "properties": { "kind": { "const": "repo_main" } },
                            "required": ["kind"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "kind": { "const": "branch" },
                                "name": { "type": "string" }
                            },
                            "required": ["kind", "name"]
                        },
                        {
                            "type": "object",
                            "properties": {
                                "kind": { "const": "commit" },
                                "oid": { "type": "string" }
                            },
                            "required": ["kind", "oid"]
                        }
                    ]
                }
            },
            "required": ["tasks"]
        }),
    }
}

fn get_plan_status_def() -> ToolDefinition {
    ToolDefinition {
        name: "get_plan_status".into(),
        description: "Get the current status of a submitted execution plan. Returns per-task status: pending (waiting for deps), ready, dispatched (running), completed, or failed. Non-blocking — returns immediately.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_id": {
                    "type": "string",
                    "description": "The plan_id returned by submit_plan"
                }
            },
            "required": ["plan_id"]
        }),
    }
}

fn get_reconciler_status_def() -> ToolDefinition {
    ToolDefinition {
        name: "get_reconciler_status".into(),
        description: "Get the reconciler's in-memory observability state across all plans, including recent dispatch outcomes, stuck tasks, and last tick time.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
    }
}

pub fn get_task_diff_def() -> ToolDefinition {
    ToolDefinition {
        name: "get_task_diff".to_string(),
        description: "Get the full unified diff for a plan task. Use after \
            get_plan_status shows tasks in awaiting_review, approved, rejected, or \
            failed state. Returns the complete diff, worker branch name, task \
            description, and summary for brain code review. Pass `attempt` to inspect \
            prior iteration attempts (see entry.history)."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_id": { "type": "string", "description": "The plan_id returned by submit_plan" },
                "task_id": { "type": "string", "description": "The task_id to inspect" },
                "attempt": {
                    "type": "integer",
                    "description": "Optional: inspect a prior attempt (1..current-1). Omit for the latest attempt."
                }
            },
            "required": ["plan_id", "task_id"]
        }),
    }
}

fn preview_task_base_def() -> ToolDefinition {
    ToolDefinition {
        name: "preview_task_base".into(),
        description: "Read-only: returns the overlay commits and predicted base OID for a given plan task without creating a worker worktree. Use this BEFORE approving a downstream task to surface integration conflicts early. Returns null `predicted_base_oid` and a `conflict` payload when overlays cannot be applied cleanly.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::PreviewTaskBaseInput>(),
    }
}

fn plan_truncate_and_restart_def() -> ToolDefinition {
    ToolDefinition {
        name: "plan_truncate_and_restart".into(),
        description: "Recovery tool for plans blocked by overlay conflicts. Cherry-picks approved task tips in DAG order onto a fresh `spur/plan-staging/{plan_id}` branch, marks remaining tasks Superseded in the original plan, and submits a new plan whose tasks dispatch against the staging branch. Use after `BlockedOnSetupConflict` when the conflict is across approved siblings (i.e. cannot be unwound by re-dispatching a single upstream task).".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::PlanTruncateAndRestartInput>(),
    }
}

fn recover_orphaned_dispatch_def() -> ToolDefinition {
    ToolDefinition {
        name: "recover_orphaned_dispatch".into(),
        description: "Brain-side recovery tool. Promote a stuck Dispatched beads task to AwaitingReview when the worker branch and dispatch base OID are known. Validates the task is still dispatched, the worker branch exists, and the branch contains exactly one commit over the dispatched base.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::RecoverOrphanedDispatchInput>(),
    }
}

pub fn review_task_def() -> ToolDefinition {
    ToolDefinition {
        name: "review_task".to_string(),
        description: "Submit a review decision for a plan task awaiting review. \
            Three decisions: 'approve' (task done, beads→closed), 'reject' (task \
            dead, beads→closed with `spur:review-rejected`, dependent tasks \
            auto-failed), or 'request_changes' (persist task back to open, clear \
            review ownership, and let the reconciler redispatch when ready — max 3 \
            attempts per task, requires `feedback`). Returns updated plan status \
            with counts and ready_to_merge flag."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "plan_id": {
                    "type": "string",
                    "description": "The plan_id returned by submit_plan"
                },
                "task_id": {
                    "type": "string",
                    "description": "The task_id to review"
                },
                "decision": {
                    "type": "string",
                    "enum": ["approve", "reject", "request_changes"],
                    "description": "Review verdict. 'approve' → task marked done and persisted closed; newly-ready work is picked up by the reconciler. 'reject' → task terminal, pending/ready dependents cascaded to failed (dispatched/awaiting_review dependents flagged in warnings). 'request_changes' → task persisted back to open so the reconciler can redispatch it later, bounded by max_attempts (3)."
                },
                "feedback": {
                    "type": "string",
                    "description": "Optional feedback. Recommended when decision is `request_changes` so the worker has context for the next attempt."
                },
                "reuse_prior_worktree": {
                    "type": "boolean",
                    "description": "When request_changes is the decision, opt in to having the prior rejected attempt's diff pre-applied as uncommitted changes in the next attempt's worktree."
                }
            },
            "required": ["plan_id", "task_id", "decision"]
        }),
    }
}

fn submit_plan_mutation_def() -> ToolDefinition {
    ToolDefinition {
        name: "submit_plan_mutation".into(),
        description: "Brain-side recovery tool. This is the 'Swiss Army knife' for \
            the brain agent to fix escalated/failed running plans. Apply an \
            atomic batch of plan-graph mutations to recover an escalated task: \
            retry it as-is, rewrite its spec (task body, agent, context, deps), \
            or abandon it. Wraps `apply_mutation` end-to-end with cycle \
            detection + rollback. Clears `signal:escalated` from every \
            affected issue on success."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["trigger_task_id", "ops"],
            "properties": {
                "trigger_task_id": {
                    "type": "string",
                    "description": "Beads issue id used as the audit anchor for the batch. Typically the escalated task."
                },
                "mutation_id": {
                    "type": "string",
                    "description": "Optional UUID; auto-generated when absent."
                },
                "ops": {
                    "type": "array",
                    "description": "Ordered list of `PlanMutationOp` JSON values. Supported tags: split_task, retry_task, modify_task_spec, abandon_task.",
                    "items": { "type": "object" }
                },
                "rationale": {
                    "type": "string",
                    "description": "Free-form explanation for the audit trail."
                }
            }
        }),
    }
}

fn report_signal_def() -> ToolDefinition {
    ToolDefinition {
        name: "report_signal".into(),
        description: "Worker-facing. Record a typed WorkerSignal on a task. \
            Brain-side watcher will inspect and may mutate the plan."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["task_id", "signal"],
            "properties": {
                "task_id": { "type": "string" },
                "signal": {
                    "type": "object",
                    "required": ["kind", "signal_id"],
                    "properties": {
                        "kind": { "type": "string", "enum": ["scope_drift", "retry_exhausted"] },
                        "signal_id": { "type": "string", "format": "uuid" },
                        // ScopeDrift fields
                        "severity": { "type": "number", "minimum": 0, "maximum": 1 },
                        "reason": { "type": "string" },
                        "estimated_subtasks": { "type": ["integer", "null"], "minimum": 1 },
                        // RetryExhausted fields (bd-2m2u Phase 2e)
                        "task_id": { "type": "string" },
                        "attempt": { "type": "integer", "minimum": 0 },
                        "last_error": { "type": "string" }
                    }
                }
            }
        }),
    }
}

fn report_progress_def() -> ToolDefinition {
    ToolDefinition {
        name: "report_progress".into(),
        description: "Worker-facing fire-and-forget progress emission. Sends \
            a free-form `message` (and optional `percent`) to the brain as a \
            `WorkerReportProgress` event. The handler returns `{ok: true}` \
            on accept; the side effect IS the event. No PM writes, no audit \
            sentinel — distinct from `report_signal` (which persists). \
            Workers stream rich progress text without minting structured \
            milestone names. Consumers (TUI / dashboards) decide how to \
            render `percent` (no clamping)."
            .into(),
        input_schema: json!({
            "type": "object",
            "required": ["message"],
            "properties": {
                "message": { "type": "string" },
                "percent": { "type": ["number", "null"] }
            }
        }),
    }
}

fn execute_epic_def() -> ToolDefinition {
    ToolDefinition {
        name: "execute_epic".into(),
        description: "Execute a beads epic: hydrate a plan from the epic's \
            children subgraph and dispatch in dependency order. Agent routing \
            comes from the `spur:agent:<name>` label on each child issue \
            (inherited from the epic if unset, or from default_agent). Task \
            text comes from issue.body. Rejects nested sub-epic children. External blocked_by \
            references must already be `done`. After dispatch, the plan runs \
            under the normal review engine — use get_plan_status / \
            get_task_diff / review_task. Re-calling while a plan is active \
            for the same epic returns the existing plan_id (idempotent). \
            After terminal state, a new call starts a fresh plan."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "epic_id": {
                    "type": "string",
                    "description": "The beads ID of an issue with type=epic"
                },
                "default_agent": {
                    "type": "string",
                    "description": "Fallback agent when a child has no `spur:agent:<name>` label and the epic has no inherited label"
                }
            },
            "required": ["epic_id"]
        }),
    }
}

pub(crate) fn legacy_prelude_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        delegate_to_worker_def(),
        delegate_parallel_def(),
        check_delegation_status_def(),
        fetch_outcome_artifact_def(),
        cancel_delegation_def(),
        list_available_workers_def(),
    ]
}

pub(crate) fn legacy_plan_management_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        merge_plan_def(),
        resume_plan_def(),
        force_reclaim_plan_def(),
    ]
}

pub(crate) fn legacy_remainder_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        submit_plan_def(),
        execute_epic_def(),
        get_plan_status_def(),
        get_reconciler_status_def(),
        get_task_diff_def(),
        preview_task_base_def(),
        plan_truncate_and_restart_def(),
        recover_orphaned_dispatch_def(),
        review_task_def(),
        submit_plan_mutation_def(),
        report_signal_def(),
        report_progress_def(),
    ]
}

/// Returns all tool definitions for the MCP `tools/list` response.
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
    let mut tools = pm_tool_definitions_by_names(&["get_issue", "list_issues"]);
    tools.extend([
        get_task_diff_def(),
        get_plan_status_def(),
        fetch_outcome_artifact_def(),
    ]);
    tools
}

pub(crate) fn legacy_worker_remainder_tool_definitions() -> Vec<ToolDefinition> {
    vec![report_signal_def(), report_progress_def()]
}

pub fn worker_tools_list() -> Vec<ToolDefinition> {
    crate::registry::default_worker_tool_registry()
        .expect("default worker MCP tool registry must be valid")
        .list_tools()
}

#[cfg(test)]
mod schema_truthfulness_tests {
    use super::*;
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

    #[test]
    fn get_issue_schema_does_not_advertise_source() {
        let def = tools_list()
            .into_iter()
            .find(|tool| tool.name == "get_issue")
            .expect("get_issue must appear in tools/list");
        assert!(
            !props_of(&def).contains(&"source".to_string()),
            "get_issue must not advertise `source` until multi-backend lands",
        );
    }

    #[test]
    fn update_issue_schema_does_not_advertise_source() {
        let def = tools_list()
            .into_iter()
            .find(|tool| tool.name == "update_issue")
            .expect("update_issue must appear in tools/list");
        assert!(
            !props_of(&def).contains(&"source".to_string()),
            "update_issue must not advertise `source` until multi-backend lands",
        );
    }

    #[test]
    fn fetch_outcome_artifact_appears_in_tools_list() {
        let tools = tools_list();
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"fetch_outcome_artifact"),
            "fetch_outcome_artifact must appear in tools/list, got: {names:?}"
        );
    }

    #[test]
    fn plan_truncate_and_restart_appears_in_tools_list() {
        let tools = tools_list();
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"plan_truncate_and_restart"),
            "plan_truncate_and_restart must appear in tools/list, got: {names:?}"
        );
    }

    #[test]
    fn recover_orphaned_dispatch_appears_in_tools_list() {
        let tools = tools_list();
        let names: Vec<_> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(
            names.contains(&"recover_orphaned_dispatch"),
            "recover_orphaned_dispatch must appear in tools/list, got: {names:?}"
        );
    }

    #[test]
    fn knowledge_context_pack_schema_matches_contract() {
        let tools = tools_list();
        let def = tools
            .iter()
            .find(|tool| tool.name == "knowledge_context_pack")
            .expect("knowledge_context_pack must appear in tools/list");
        let props = def
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("properties");
        let stale_ann_boundary = ["Lance ANN is", "not used by this MVP"].join(" ");

        assert!(
            !def.description.contains(&stale_ann_boundary)
                && def
                    .description
                    .contains("opportunistic Lance hybrid vector re-ranking")
                && def.description.contains("degrades to BM25-only")
                && def.description.contains("code_read_symbol/code_callers/code_callees"),
            "knowledge_context_pack description must state opportunistic Lance fallback and exact graph follow-ups"
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
                "include_tests",
                "intent",
                "limit",
                "max_symbol_bodies",
                "query",
                "scope",
            ],
            "knowledge_context_pack property set drifted",
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
        let tools = tools_list();
        let def = tools
            .iter()
            .find(|tool| tool.name == "knowledge_context_pack_2")
            .expect("knowledge_context_pack_2 must appear in tools/list");
        let props = def
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("properties");

        assert!(
            def.description.contains("structured evidence pack")
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

        let error = crate::server::handlers::code_graph::code_callers(&json!({}))
            .await
            .expect_err("handler must reject calls without selector or symbol");
        assert_eq!(error.json_rpc_code(), -32602);
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

    #[test]
    fn fetch_outcome_artifact_schema_advertises_phase3_sections() {
        let def = fetch_outcome_artifact_def();
        let props = def
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("properties");
        let section = props.get("section").expect("section property");
        let enum_values: Vec<&str> = section
            .get("enum")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert_eq!(
            enum_values,
            vec!["status_only", "summary", "diff_only", "full"],
            "Phase 3 must advertise all fetchable sections"
        );

        let required: Vec<&str> = def
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        assert!(
            required.contains(&"delegation_id"),
            "delegation_id must be required"
        );
    }
}

#[cfg(test)]
mod worker_tools_subset_tests {
    use super::*;

    const EXPECTED_ALL_TOOLS: &[&str] = &[
        "delegate_to_worker",
        "delegate_parallel",
        "check_delegation_status",
        "fetch_outcome_artifact",
        "cancel_delegation",
        "list_available_workers",
        "get_issue",
        "list_issues",
        "update_issue",
        "create_issue",
        "add_dependency",
        "create_pr",
        "merge_plan",
        "resume_plan",
        "force_reclaim_plan",
        "graph_triage",
        "graph_plan",
        "graph_insights",
        "graph_alerts",
        "graph_subgraph",
        "code_resolve",
        "code_file_symbols",
        "code_symbol_info",
        "code_read_symbol",
        "code_callers",
        "code_callees",
        "code_symbol_search",
        "code_subgraph",
        "code_symbol_history",
        "doc_navigate",
        "knowledge_context_pack",
        "knowledge_context_pack_2",
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
        "report_signal",
        "report_progress",
    ];

    const EXPECTED_WORKER_TOOLS: &[&str] = &[
        "get_issue",
        "list_issues",
        "get_task_diff",
        "get_plan_status",
        "fetch_outcome_artifact",
        "code_resolve",
        "code_file_symbols",
        "code_symbol_info",
        "code_read_symbol",
        "code_callers",
        "code_callees",
        "code_symbol_search",
        "code_subgraph",
        "code_symbol_history",
        "doc_navigate",
        "knowledge_context_pack",
        "knowledge_context_pack_2",
        "report_signal",
        "report_progress",
    ];

    #[test]
    fn tools_list_contains_exactly_the_compatibility_set() {
        let actual: Vec<String> = tools_list().iter().map(|t| t.name.clone()).collect();
        let expected: Vec<String> = EXPECTED_ALL_TOOLS.iter().map(|s| s.to_string()).collect();
        assert_eq!(
            actual, expected,
            "tools_list drift; update EXPECTED_ALL_TOOLS in same commit if intentional",
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
        assert_eq!(
            registry.canonical_name("code_search"),
            Some("code_symbol_search")
        );
    }

    #[test]
    fn knowledge_context_pack_appears_in_worker_tools_list() {
        let actual: Vec<String> = worker_tools_list()
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
    fn worker_tools_list_contains_exactly_the_curated_set() {
        let actual: Vec<String> = worker_tools_list().iter().map(|t| t.name.clone()).collect();
        let expected: Vec<String> = EXPECTED_WORKER_TOOLS
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            actual, expected,
            "worker_tools_list drift; update EXPECTED_WORKER_TOOLS in same commit if intentional",
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
        for w in worker_tools_list() {
            assert!(
                full.contains(&w.name),
                "worker tool '{}' missing from full tools_list — definitions must align",
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
