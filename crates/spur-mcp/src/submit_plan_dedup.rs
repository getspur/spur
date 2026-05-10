use anyhow::Context;
use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};
use spur_pm::{IssueCreate, IssueFilter, PmService};
use tracing::{info, warn};

const REGISTRY_LABEL: &str = "spur:dedup";
const KEY_LABEL_PREFIX: &str = "spur:dedup-key:";
const KEY_HASH_HEX_CHARS: usize = 32;
const TTL: Duration = Duration::hours(24);

pub(crate) struct DedupHit {
    pub(crate) plan_id: String,
    pub(crate) issue_id: String,
}

pub(crate) fn key_label(key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    format!("{KEY_LABEL_PREFIX}{}", &hash[..KEY_HASH_HEX_CHARS])
}

pub(crate) async fn lookup(pm: &PmService, key: &str) -> anyhow::Result<Option<DedupHit>> {
    let label = key_label(key);
    let summaries = pm
        .list_issues(IssueFilter {
            labels: vec![label.clone()],
            include_closed: true,
            limit: Some(100),
            ..IssueFilter::default()
        })
        .await
        .with_context(|| format!("list dedup entries for label {label}"))?;

    let mut issues = Vec::new();
    for summary in summaries {
        let issue = pm
            .get_issue(&summary.id)
            .await
            .with_context(|| format!("load dedup entry {}", summary.id))?;
        issues.push(issue);
    }
    issues.sort_by(|left, right| right.created_at.cmp(&left.created_at));

    let now = Utc::now();
    for issue in issues {
        let age = now.signed_duration_since(issue.created_at);
        if age > TTL {
            warn!(
                dedup_issue_id = %issue.id,
                age_seconds = age.num_seconds(),
                ttl_seconds = TTL.num_seconds(),
                "submit_plan dedup entry is older than TTL; ignoring"
            );
            continue;
        }

        let plan_id = issue.body.trim();
        if plan_id.is_empty() {
            warn!(
                dedup_issue_id = %issue.id,
                "submit_plan dedup entry has empty body; ignoring"
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

pub(crate) async fn record(pm: &PmService, key: &str, plan_id: &str) -> anyhow::Result<String> {
    if let Some(hit) = lookup(pm, key).await? {
        return Ok(hit.issue_id);
    }

    let registry_id = registry_epic(pm).await?;
    let label = key_label(key);
    let issue_id = pm
        .create_issue(IssueCreate {
            title: format!("submit_plan dedup {label}"),
            description: Some(plan_id.to_string()),
            issue_type: Some("task".to_string()),
            labels: vec![label.clone()],
            parent: Some(registry_id.clone()),
            ..IssueCreate::default()
        })
        .await
        .with_context(|| format!("create submit_plan dedup entry for label {label}"))?;

    info!(
        dedup_issue_id = %issue_id,
        dedup_registry_id = %registry_id,
        plan_id = %plan_id,
        "submit_plan dedup entry recorded"
    );
    Ok(issue_id)
}

async fn registry_epic(pm: &PmService) -> anyhow::Result<String> {
    let summaries = pm
        .list_issues(IssueFilter {
            labels: vec![REGISTRY_LABEL.to_string()],
            include_closed: true,
            limit: Some(10),
            ..IssueFilter::default()
        })
        .await
        .context("list submit_plan dedup registry epics")?;

    if let Some(summary) = summaries
        .iter()
        .find(|issue| issue.issue_type.as_deref() == Some("epic"))
        .or_else(|| summaries.first())
    {
        return Ok(summary.id.clone());
    }

    pm.create_issue(IssueCreate {
        title: "SPUR submit_plan dedup registry".to_string(),
        description: Some(
            "Synthetic registry for submit_plan client_idempotency_key dedup entries.".to_string(),
        ),
        issue_type: Some("epic".to_string()),
        labels: vec![REGISTRY_LABEL.to_string()],
        ..IssueCreate::default()
    })
    .await
    .context("create submit_plan dedup registry epic")
}
