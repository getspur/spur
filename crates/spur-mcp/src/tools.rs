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
    /// Phase 5 / Task 26 — opt-in to the curated worker MCP subset.
    /// `None` or `Some(false)` preserves the historical "Workers get no
    /// MCP servers" contract; `Some(true)` triggers the orchestrator's
    /// per-`BrainSession` `WorkerMcpServer` boot and a 1-hour HMAC
    /// token URL injection into the worker's `mcp_servers` config.
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

// ─── Tool definitions ─────────────────────────────────────────────────

fn delegate_to_worker_def() -> ToolDefinition {
    ToolDefinition {
        name: "delegate_to_worker".into(),
        description: "Delegate a task to a worker agent. Returns inline if the worker finishes within the inline-wait window (configurable via `delegation.inline_wait_ms`; default 0). Otherwise returns `{status: \"pending\", delegation_id}` and you will be re-prompted automatically when the worker completes — you do not need to poll. Pass a `delegation_plan` parameter (at minimum `{chosen, rationale}`; more for multi-step work). Structure the `task` field as CONTEXT / GOAL / CONSTRAINTS / EXPECTED_OUTPUT. Optional `enable_worker_mcp` and `enable_worker_progress` booleans default to false/omitted; workers receive no MCP or progress channel unless explicitly opted in. Use `list_available_workers` when routing is ambiguous.".into(),
        input_schema: crate::tool_schemas::schema_value::<crate::tool_schemas::DelegateToWorkerInput>(),
    }
}

fn delegate_parallel_def() -> ToolDefinition {
    ToolDefinition {
        name: "delegate_parallel".into(),
        description: "Delegate multiple tasks in parallel. Returns a response array of length N; each element is either an inline result or `{status: \"pending\", delegation_id}` with an automatic re-prompt when that task completes. Each task's per-task `delegation_plan` documents structured reasoning for reviewer mismatch checks. Per-task optional `enable_worker_mcp` and `enable_worker_progress` booleans default to false/omitted; workers receive no MCP or progress channel unless explicitly opted in. Subtasks MUST be independent — no shared state, no sequential data dependencies. If unsure, use `delegate_to_worker` serially.".into(),
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

fn get_issue_def() -> ToolDefinition {
    ToolDefinition {
        name: "get_issue".into(),
        description: "Retrieve an issue from the configured project management backend (beads, GitHub, etc.)."
            .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Issue identifier"
                }
            },
            "required": ["id"]
        }),
    }
}

fn list_issues_def() -> ToolDefinition {
    ToolDefinition {
        name: "list_issues".into(),
        description:
            "List issues from the configured project management backend with optional filters."
                .into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "description": "Filter by status: open, in_progress, blocked, closed"
                },
                "labels": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Filter by labels (issues must have all listed labels)"
                },
                "assignee": {
                    "type": "string",
                    "description": "Filter by assignee username"
                },
                "priority_min": {
                    "type": "integer",
                    "description": "Minimum priority (0=critical, 4=backlog)"
                },
                "priority_max": {
                    "type": "integer",
                    "description": "Maximum priority (0=critical, 4=backlog)"
                },
                "issue_type": {
                    "type": "string",
                    "description": "Filter by issue type: task, bug, feature, epic"
                },
                "text_search": {
                    "type": "string",
                    "description": "Search issue titles by text"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default 20, max 100)"
                }
            }
        }),
    }
}

fn update_issue_def() -> ToolDefinition {
    ToolDefinition {
        name: "update_issue".into(),
        description: "Update an issue in the configured project management backend.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Issue identifier"
                },
                "status": {
                    "type": "string",
                    "description": "New status to set"
                },
                "comment": {
                    "type": "string",
                    "description": "Comment to add to the issue"
                },
                "priority": {
                    "type": "integer",
                    "description": "New priority (0=critical, 4=backlog)"
                },
                "assignee": {
                    "type": "string",
                    "description": "Assignee username. Use empty string to unassign."
                },
                "add_labels": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Labels to add to the issue"
                },
                "remove_labels": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Labels to remove from the issue"
                }
            },
            "required": ["id"]
        }),
    }
}

fn create_pr_def() -> ToolDefinition {
    ToolDefinition {
        name: "create_pr".into(),
        description: "Create a pull request.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "PR title"
                },
                "body": {
                    "type": "string",
                    "description": "PR description body"
                },
                "branch": {
                    "type": "string",
                    "description": "Head branch name"
                },
                "base_branch": {
                    "type": "string",
                    "description": "Base branch to merge into (optional, defaults to repo default)"
                },
                "repo": {
                    "type": "string",
                    "description": "Repository identifier (optional, for multi-repo setups)"
                }
            },
            "required": ["title", "body", "branch"]
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
                    "description": "Which section to fetch. Phase 3."
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

// ─── Graph analysis tool definitions (in-process graph engine) ──────

fn graph_triage_def() -> ToolDefinition {
    ToolDefinition {
        name: "graph_triage".into(),
        description: "Get PageRank-weighted project triage: top recommendations, quick wins, blockers to clear, and project health metrics. Complements list_issues (CRUD) with graph-based dependency analysis. Call this FIRST for orientation before starting work. Note: triage includes alert data — only call graph_alerts separately when you need standalone alert monitoring without the full triage context. Optionally scope to a label.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "label": {
                    "type": "string",
                    "description": "Scope analysis to issues with this label"
                }
            }
        }),
    }
}

fn graph_plan_def() -> ToolDefinition {
    ToolDefinition {
        name: "graph_plan".into(),
        description: "Get a dependency-aware parallel execution plan. Returns independent tracks of work that can proceed simultaneously. Use to identify what can be delegated in parallel.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "label": {
                    "type": "string",
                    "description": "Scope to issues with this label"
                }
            }
        }),
    }
}

fn graph_insights_def() -> ToolDefinition {
    ToolDefinition {
        name: "graph_insights".into(),
        description: "Get full graph metrics: PageRank (importance), betweenness (bottlenecks), HITS (hubs/authorities), critical path, cycles, articulation points. Use for deep structural analysis of the project dependency graph.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "label": {
                    "type": "string",
                    "description": "Scope to issues with this label"
                }
            }
        }),
    }
}

fn graph_alerts_def() -> ToolDefinition {
    ToolDefinition {
        name: "graph_alerts".into(),
        description: "Get active health alerts: stale issues, blocking cascades, priority mismatches, circular dependencies. Use for project health monitoring.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {}
        }),
    }
}

fn graph_subgraph_def() -> ToolDefinition {
    ToolDefinition {
        name: "graph_subgraph".into(),
        description: "Get the dependency subgraph for a specific issue. Returns nodes and edges showing what this issue depends on and what depends on it. Use format=mermaid for visual dependency diagrams.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "root_id": {
                    "type": "string",
                    "description": "Issue ID to center the subgraph on"
                },
                "depth": {
                    "type": "integer",
                    "description": "Max traversal depth (0 = unlimited, default)"
                },
                "format": {
                    "type": "string",
                    "enum": ["json", "dot", "mermaid"],
                    "description": "Output format (default: json)"
                }
            },
            "required": ["root_id"]
        }),
    }
}

// ─── Issue creation + dependency tools ────────────────────────────

fn create_issue_def() -> ToolDefinition {
    ToolDefinition {
        name: "create_issue".into(),
        description: "Create a new issue in the project tracker. For task decomposition: create an epic first (type=epic), then create child tasks with parent=<epic_id>. Structure descriptions as CONTEXT / GOAL / CONSTRAINTS / ACCEPTANCE CRITERIA. Use depends_on to wire dependency edges so graph_plan can compute optimal execution ordering. After creating all tasks, call graph_plan() to get dependency-aware tracks, then submit_plan() to execute.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Issue title — concise, action-oriented"
                },
                "description": {
                    "type": "string",
                    "description": "Detailed description (markdown). Structure as CONTEXT / GOAL / CONSTRAINTS / ACCEPTANCE CRITERIA for tasks."
                },
                "type": {
                    "type": "string",
                    "enum": ["task", "bug", "feature", "epic"],
                    "description": "Issue type. Use 'epic' for grouping, 'task' for work items."
                },
                "priority": {
                    "type": "integer",
                    "description": "Priority 0-4 (0=critical, 4=backlog). Affects graph_triage ranking."
                },
                "labels": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Labels for categorization"
                },
                "parent": {
                    "type": "string",
                    "description": "Parent issue ID — creates parent-child dependency (e.g., epic ID for child tasks)"
                },
                "assignee": {
                    "type": "string",
                    "description": "Assignee username"
                },
                "estimate": {
                    "type": "integer",
                    "description": "Time estimate in minutes"
                },
                "depends_on": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Issue IDs this new issue depends on (blocking deps). Used by graph_plan for execution ordering."
                }
            },
            "required": ["title"]
        }),
    }
}

fn add_dependency_def() -> ToolDefinition {
    ToolDefinition {
        name: "add_dependency".into(),
        description: "Add a dependency edge between existing issues: issue_id is blocked by depends_on_id. Use after creating issues to wire the dependency graph. After wiring deps, call graph_plan() to get optimized execution ordering. Beads backend only — returns error for GitHub.".into(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "issue_id": {
                    "type": "string",
                    "description": "The issue that is blocked"
                },
                "depends_on_id": {
                    "type": "string",
                    "description": "The issue that blocks it (must complete first)"
                }
            },
            "required": ["issue_id", "depends_on_id"]
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
                    "description": "Epic title. Required unless a non-empty first task description can derive one."
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
                    "description": "Review notes. Required for all decisions — used as rationale for approve/reject and as retry instruction for request_changes."
                }
            },
            "required": ["plan_id", "task_id", "decision", "feedback"]
        }),
    }
}

fn submit_plan_mutation_def() -> ToolDefinition {
    ToolDefinition {
        name: "submit_plan_mutation".into(),
        description: "Brain-side recovery tool. Apply an atomic batch of \
            plan-graph mutations to recover an escalated/failed task: retry \
            it as-is, rewrite its spec (task body, agent, context, deps), \
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

/// Returns all tool definitions for the MCP `tools/list` response.
pub fn tools_list() -> Vec<ToolDefinition> {
    vec![
        delegate_to_worker_def(),
        delegate_parallel_def(),
        check_delegation_status_def(),
        fetch_outcome_artifact_def(),
        cancel_delegation_def(),
        list_available_workers_def(),
        get_issue_def(),
        list_issues_def(),
        update_issue_def(),
        create_issue_def(),
        add_dependency_def(),
        create_pr_def(),
        merge_plan_def(),
        resume_plan_def(),
        force_reclaim_plan_def(),
        graph_triage_def(),
        graph_plan_def(),
        graph_insights_def(),
        graph_alerts_def(),
        graph_subgraph_def(),
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

/// Curated worker-facing tool subset exposed by `WorkerMcpServer`.
///
/// Workers receive only read-and-emit tools: 5 read tools that surface
/// project state plus 1 write tool (`update_issue`) and 2 fire-and-forget
/// signal tools (`report_signal`, `report_progress`). Brain-only orchestration
/// tools (delegate_*, submit_plan, merge_plan, execute_epic, review_task,
/// create_pr, create_issue, add_dependency, cancel_delegation,
/// list_available_workers, get_reconciler_status, graph_*, preview_task_base,
/// plan_truncate_and_restart) are intentionally excluded — exposing them to
/// workers would invert the brain→worker authority direction and let workers
/// self-dispatch.
pub fn worker_tools_list() -> Vec<ToolDefinition> {
    vec![
        get_issue_def(),
        list_issues_def(),
        get_task_diff_def(),
        get_plan_status_def(),
        fetch_outcome_artifact_def(),
        update_issue_def(),
        report_signal_def(),
        report_progress_def(),
    ]
}

#[cfg(test)]
mod schema_truthfulness_tests {
    use super::*;

    fn props_of(def: &ToolDefinition) -> Vec<String> {
        def.input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default()
    }

    #[test]
    fn get_issue_schema_does_not_advertise_source() {
        let def = get_issue_def();
        assert!(
            !props_of(&def).contains(&"source".to_string()),
            "get_issue must not advertise `source` until multi-backend lands",
        );
    }

    #[test]
    fn update_issue_schema_does_not_advertise_source() {
        let def = update_issue_def();
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

    const EXPECTED_WORKER_TOOLS: &[&str] = &[
        "get_issue",
        "list_issues",
        "get_task_diff",
        "get_plan_status",
        "fetch_outcome_artifact",
        "update_issue",
        "report_signal",
        "report_progress",
    ];

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
        for tool in &forbidden {
            assert!(
                !actual.iter().any(|n| n == tool),
                "leaked brain-only tool into worker subset: {tool}",
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
