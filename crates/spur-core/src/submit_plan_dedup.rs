use anyhow::Context;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use spur_pm::{IssueCreate, IssueFilter};
use tracing::{info, warn};

const REGISTRY_LABEL: &str = "spur:dedup";
const KEY_LABEL_PREFIX: &str = "spur:dedup-key:";
const LOOP_REGISTRY_LABEL: &str = "spur:dedup-loop";
const LOOP_KEY_LABEL_PREFIX: &str = "spur:dedup-lkey:";
const KEY_HASH_HEX_CHARS: usize = 32;
const TTL: Duration = Duration::hours(24);

#[derive(Clone, Copy)]
enum DedupKind {
    SubmitPlan,
    SubmitLoop,
}

impl DedupKind {
    fn action_name(self) -> &'static str {
        match self {
            Self::SubmitPlan => "submit_plan",
            Self::SubmitLoop => "submit_loop",
        }
    }

    fn registry_label(self) -> &'static str {
        match self {
            Self::SubmitPlan => REGISTRY_LABEL,
            Self::SubmitLoop => LOOP_REGISTRY_LABEL,
        }
    }

    fn key_label_prefix(self) -> &'static str {
        match self {
            Self::SubmitPlan => KEY_LABEL_PREFIX,
            Self::SubmitLoop => LOOP_KEY_LABEL_PREFIX,
        }
    }

    fn registry_title(self) -> &'static str {
        match self {
            Self::SubmitPlan => "SPUR submit_plan dedup registry",
            Self::SubmitLoop => "SPUR submit_loop dedup registry",
        }
    }

    fn registry_description(self) -> &'static str {
        match self {
            Self::SubmitPlan => {
                "Synthetic registry for submit_plan client_idempotency_key dedup entries."
            }
            Self::SubmitLoop => {
                "Synthetic registry for submit_loop client_idempotency_key dedup entries."
            }
        }
    }
}

pub(crate) struct DedupHit {
    pub(crate) plan_id: String,
    pub(crate) issue_id: String,
}

pub(crate) struct LoopDedupHit {
    pub(crate) loop_id: String,
    pub(crate) issue_id: String,
    pub(crate) next_run: i64,
    pub(crate) paused: bool,
    pub(crate) dedup_issue_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct LoopDedupBody {
    loop_id: String,
    issue_id: String,
    next_run: i64,
    paused: bool,
}

pub(crate) fn key_label(key: &str) -> String {
    key_label_for(DedupKind::SubmitPlan, key)
}

fn key_label_for(kind: DedupKind, key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    format!("{}{}", kind.key_label_prefix(), &hash[..KEY_HASH_HEX_CHARS])
}

pub(crate) async fn lookup(
    pm: &dyn crate::plan::PmLike,
    key: &str,
) -> anyhow::Result<Option<DedupHit>> {
    lookup_for(pm, DedupKind::SubmitPlan, key).await
}

async fn lookup_for(
    pm: &dyn crate::plan::PmLike,
    kind: DedupKind,
    key: &str,
) -> anyhow::Result<Option<DedupHit>> {
    let label = match kind {
        DedupKind::SubmitPlan => key_label(key),
        DedupKind::SubmitLoop => key_label_for(kind, key),
    };
    let summaries = pm
        .list_issues(IssueFilter {
            labels: vec![label.clone()],
            include_closed: true,
            limit: Some(100),
            ..IssueFilter::default()
        })
        .await
        .with_context(|| {
            format!(
                "list {} dedup entries for label {label}",
                kind.action_name()
            )
        })?;

    let mut issues = Vec::new();
    for summary in summaries {
        let issue = pm
            .get_issue(&summary.id)
            .await
            .with_context(|| format!("load dedup entry {}", summary.id))?;
        issues.push(issue);
    }
    issues.sort_by_key(|issue| std::cmp::Reverse(issue.created_at));

    let now = Utc::now();
    for issue in issues {
        let age = now.signed_duration_since(issue.created_at);
        if age > TTL {
            warn!(
                dedup_kind = kind.action_name(),
                dedup_issue_id = %issue.id,
                age_seconds = age.num_seconds(),
                ttl_seconds = TTL.num_seconds(),
                "dedup entry is older than TTL; ignoring"
            );
            continue;
        }

        let plan_id = issue.body.trim();
        if plan_id.is_empty() {
            warn!(
                dedup_kind = kind.action_name(),
                dedup_issue_id = %issue.id,
                "dedup entry has empty body; ignoring"
            );
            continue;
        }

        return Ok(Some(DedupHit {
            plan_id: plan_id.to_string(),
            issue_id: issue.id,
        }));
    }

    Ok(None)
}

pub(crate) async fn record(
    pm: &dyn crate::plan::PmLike,
    key: &str,
    plan_id: &str,
) -> anyhow::Result<String> {
    record_for(pm, DedupKind::SubmitPlan, key, plan_id).await
}

async fn record_for(
    pm: &dyn crate::plan::PmLike,
    kind: DedupKind,
    key: &str,
    body: &str,
) -> anyhow::Result<String> {
    if let Some(hit) = lookup_for(pm, kind, key).await? {
        return Ok(hit.issue_id);
    }

    let registry_id = registry_epic(pm, kind).await?;
    let label = key_label_for(kind, key);
    let issue_id = pm
        .create_issue(IssueCreate {
            title: format!("{} dedup {label}", kind.action_name()),
            description: Some(body.to_string()),
            issue_type: Some("task".to_string()),
            labels: vec![label.clone()],
            parent: Some(registry_id.clone()),
            ..IssueCreate::default()
        })
        .await
        .with_context(|| {
            format!(
                "create {} dedup entry for label {label}",
                kind.action_name()
            )
        })?;

    info!(
        dedup_kind = kind.action_name(),
        dedup_issue_id = %issue_id,
        dedup_registry_id = %registry_id,
        "dedup entry recorded"
    );
    Ok(issue_id)
}

pub(crate) async fn lookup_loop(
    pm: &dyn crate::plan::PmLike,
    key: &str,
) -> anyhow::Result<Option<LoopDedupHit>> {
    let Some(hit) = lookup_for(pm, DedupKind::SubmitLoop, key).await? else {
        return Ok(None);
    };
    let body = match serde_json::from_str::<LoopDedupBody>(&hit.plan_id) {
        Ok(body) => body,
        Err(error) => {
            warn!(
                dedup_issue_id = %hit.issue_id,
                "submit_loop dedup entry has invalid body; ignoring: {error}"
            );
            return Ok(None);
        }
    };
    if body.loop_id.trim().is_empty() || body.issue_id.trim().is_empty() {
        warn!(
            dedup_issue_id = %hit.issue_id,
            "submit_loop dedup entry is missing loop_id or issue_id; ignoring"
        );
        return Ok(None);
    }

    Ok(Some(LoopDedupHit {
        loop_id: body.loop_id,
        issue_id: body.issue_id,
        next_run: body.next_run,
        paused: body.paused,
        dedup_issue_id: hit.issue_id,
    }))
}

pub(crate) async fn record_loop(
    pm: &dyn crate::plan::PmLike,
    key: &str,
    loop_id: &str,
    issue_id: &str,
    next_run: i64,
    paused: bool,
) -> anyhow::Result<String> {
    let body = serde_json::to_string(&LoopDedupBody {
        loop_id: loop_id.to_string(),
        issue_id: issue_id.to_string(),
        next_run,
        paused,
    })
    .context("serialize submit_loop dedup entry")?;
    record_for(pm, DedupKind::SubmitLoop, key, &body).await
}

async fn registry_epic(pm: &dyn crate::plan::PmLike, kind: DedupKind) -> anyhow::Result<String> {
    let summaries = pm
        .list_issues(IssueFilter {
            labels: vec![kind.registry_label().to_string()],
            include_closed: true,
            limit: Some(10),
            ..IssueFilter::default()
        })
        .await
        .with_context(|| format!("list {} dedup registry epics", kind.action_name()))?;

    if let Some(summary) = summaries
        .iter()
        .find(|issue| issue.issue_type.as_deref() == Some("epic"))
        .or_else(|| summaries.first())
    {
        return Ok(summary.id.clone());
    }

    pm.create_issue(IssueCreate {
        title: kind.registry_title().to_string(),
        description: Some(kind.registry_description().to_string()),
        issue_type: Some("epic".to_string()),
        labels: vec![kind.registry_label().to_string()],
        ..IssueCreate::default()
    })
    .await
    .with_context(|| format!("create {} dedup registry epic", kind.action_name()))
}
