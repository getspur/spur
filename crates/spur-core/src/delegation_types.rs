use std::sync::atomic::AtomicU32;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use spur_acp::{BrainSessionId, DelegationId, DelegationPlan, DelegationResult};
use tokio::sync::{oneshot, watch};

/// Where a worker's worktree should be based, before any overlays.
///
/// Non-recursive sum type. Used as the inner `base` of `BaseSpec::WithOverlay`
/// to enforce that overlay chains cannot nest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BaseTarget {
    /// Snapshot from the orchestrator's repo_root HEAD.
    RepoMain,
    /// Branch by name.
    Branch { name: String },
    /// Pinned commit OID.
    Commit { oid: String },
}

impl<'de> Deserialize<'de> for BaseTarget {
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
            Inner::RepoMain => Self::RepoMain,
            Inner::Branch { name } => Self::Branch { name },
            Inner::Commit { oid } => Self::Commit { oid },
        })
    }
}

/// Where a worker's worktree should be based.
///
/// Optional on delegate tool inputs for backwards compatibility: callers that
/// omit `base` get the legacy behavior, equivalent to `BaseSpec::RepoMain`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BaseSpec {
    /// Snapshot from the orchestrator's repo_root HEAD.
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

impl<'de> Deserialize<'de> for BaseSpec {
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
            Inner::RepoMain => Self::RepoMain,
            Inner::Branch { name } => Self::Branch { name },
            Inner::Commit { oid } => Self::Commit { oid },
            Inner::WithOverlay { base, overlays } => Self::WithOverlay { base, overlays },
        })
    }
}

/// One overlay commit range to cherry-pick onto a base.
///
/// `base_oid..tip_oid` is the exclusive-of-base range.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
pub struct OverlayCommit {
    /// The plan task whose work this overlay represents.
    pub source_task_id: String,
    /// Inclusive lower bound, exclusive in the cherry-pick range.
    pub base_oid: String,
    /// Inclusive upper bound.
    pub tip_oid: String,
}

/// A delegation request sent from the core MCP module to the orchestrator.
#[derive(Debug)]
pub struct DelegationRequest {
    pub id: DelegationId,
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub task: String,
    pub context_files: Vec<String>,
    pub prior_branch_for_reuse: Option<String>,
    pub respond_to: oneshot::Sender<DelegationResult>,
    pub brain_session_id: BrainSessionId,
    pub delegation_plan: Option<DelegationPlan>,
    pub issue_id: Option<String>,
    pub base: Option<BaseSpec>,
    pub dispatched_base_oid_tx: Option<watch::Sender<Option<String>>>,
    pub attempt_tracker: Arc<AtomicU32>,
    pub enable_worker_mcp: Option<bool>,
}

/// Channel the orchestrator holds to receive core MCP delegation requests.
pub struct DelegationChannel {
    pub request_rx: tokio::sync::mpsc::Receiver<DelegationRequest>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basespec_accepts_stringified_repo_main() {
        let v = Value::String(r#"{"kind":"repo_main"}"#.into());
        let parsed: BaseSpec = serde_json::from_value(v).unwrap();
        assert_eq!(parsed, BaseSpec::RepoMain);
    }

    #[test]
    fn basespec_accepts_stringified_with_overlay_and_nested_base() {
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
    fn basetarget_round_trips() {
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
