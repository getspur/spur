//! `IssueTracker` trait impl for `BeadsCrateAdapter`.

use std::collections::HashSet;
use std::str::FromStr;

use async_trait::async_trait;
use chrono::Utc;

use crate::adapter::IssueTracker;
use crate::beads_crate::adapter::BeadsCrateAdapter;
use crate::poll_cursor::{PollCursor, POLL_FETCH_LIMIT};
use crate::types::{Issue, IssueCreate, IssueFilter, IssueSummary, IssueUpdate, PmEvent, PmSource};

fn validate_create_label(label: &str) -> anyhow::Result<()> {
    beads_rust::validation::LabelValidator::validate(label)
        .map_err(|error| anyhow::anyhow!("Validation failed: {error}"))
}

fn validate_create_labels<'a>(labels: impl IntoIterator<Item = &'a String>) -> anyhow::Result<()> {
    for label in labels {
        validate_create_label(label)?;
    }
    Ok(())
}

fn validate_added_label(label: &str) -> anyhow::Result<()> {
    if label.is_empty() {
        anyhow::bail!("Validation failed: label: cannot be empty");
    }
    if !label
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':')
    {
        anyhow::bail!(
            "Validation failed: label: invalid characters (only alphanumeric, hyphen, underscore, colon allowed)"
        );
    }
    Ok(())
}

fn validate_added_labels<'a>(labels: impl IntoIterator<Item = &'a String>) -> anyhow::Result<()> {
    for label in labels {
        validate_added_label(label)?;
    }
    Ok(())
}

pub(crate) fn br_to_pm_issue(br: beads_rust::model::Issue) -> Issue {
    let url = format!("beads://{}", br.id);
    let blocked_by = br
        .dependencies
        .iter()
        .filter(|dependency| dependency.dep_type.is_blocking())
        .map(|dependency| dependency.depends_on_id.clone())
        .collect();
    Issue {
        id: br.id,
        source: PmSource::Beads,
        title: br.title,
        body: br.description.unwrap_or_default(),
        status: br.status.to_string(),
        labels: br.labels,
        assignee: br.assignee,
        url,
        priority: Some(br.priority.0),
        issue_type: Some(br.issue_type.to_string()),
        blocked_by,
        due_at: br.due_at,
        source_system: br.source_system,
        source_repo: br.source_repo,
        external_ref: br.external_ref,
        created_at: br.created_at,
        updated_at: br.updated_at,
    }
}

pub(crate) fn br_to_pm_summary(br: beads_rust::model::Issue) -> IssueSummary {
    let url = format!("beads://{}", br.id);
    IssueSummary {
        id: br.id,
        source: PmSource::Beads,
        title: br.title,
        status: br.status.to_string(),
        labels: br.labels,
        url,
        priority: Some(br.priority.0),
        issue_type: Some(br.issue_type.to_string()),
        assignee: br.assignee,
        description: br.description,
    }
}

impl BeadsCrateAdapter {
    pub(crate) async fn poll_with_limit(&self, limit: usize) -> anyhow::Result<Vec<PmEvent>> {
        let _cursor_trace = crate::lock_trace::LockTraceGuard::lock(
            "beads.cursor",
            "BeadsCrateAdapter::poll_with_limit",
        );
        let mut guard = self.cursor.lock().await;
        let prior_cursor = guard.clone();
        let had_prior = prior_cursor.is_some();

        // Mirror BeadsAdapter::poll_with_limit: pull bounded open set,
        // apply the boundary-safe predicate client-side, advance cursor.
        let summaries = self
            .list_issues(IssueFilter {
                status: Some("open".to_string()),
                limit: Some(limit),
                ..Default::default()
            })
            .await?;

        let saturated = summaries.len() == limit;

        let mut kept: Vec<Issue> = Vec::with_capacity(summaries.len());
        for sum in summaries {
            let issue = self.get_issue(&sum.id).await?;
            let pass = match prior_cursor.as_ref() {
                None => true,
                Some(c) => c.allows(&issue.id, issue.updated_at),
            };
            if pass {
                kept.push(issue);
            }
        }

        let new_cursor: Option<PollCursor> = if !kept.is_empty() {
            if saturated {
                tracing::warn!(
                    limit,
                    kept_count = kept.len(),
                    "BeadsCrateAdapter::poll fetch saturated; preserving cursor"
                );
                prior_cursor.clone()
            } else {
                let max_ts = kept.iter().map(|i| i.updated_at).max().unwrap();
                let ids_at_max: HashSet<String> = kept
                    .iter()
                    .filter(|i| i.updated_at == max_ts)
                    .map(|i| i.id.clone())
                    .collect();
                Some(PollCursor {
                    ts: max_ts,
                    ids_at_boundary: ids_at_max,
                })
            }
        } else if let Some(existing) = prior_cursor.clone() {
            Some(existing)
        } else {
            Some(PollCursor {
                ts: Utc::now(),
                ids_at_boundary: HashSet::new(),
            })
        };

        let events: Vec<PmEvent> = kept
            .into_iter()
            .map(|issue| {
                let summary = IssueSummary {
                    id: issue.id.clone(),
                    source: PmSource::Beads,
                    title: issue.title,
                    status: issue.status,
                    labels: issue.labels,
                    url: issue.url,
                    priority: issue.priority,
                    issue_type: issue.issue_type,
                    assignee: issue.assignee,
                    description: Some(issue.body).filter(|b| !b.trim().is_empty()),
                };
                if had_prior {
                    PmEvent::IssueUpdated(summary)
                } else {
                    PmEvent::IssueCreated(summary)
                }
            })
            .collect();

        let cursor_to_persist = new_cursor.clone();
        *guard = new_cursor;
        drop(guard);

        if let (Some(path), Some(cursor)) =
            (self.config.cursor_path.as_ref(), cursor_to_persist.as_ref())
        {
            if let Err(e) = cursor.write_to(path) {
                tracing::warn!(?path, "failed to write cursor file: {e}");
            }
        }

        Ok(events)
    }
}

#[async_trait]
impl IssueTracker for BeadsCrateAdapter {
    async fn get_issue(&self, id: &str) -> anyhow::Result<Issue> {
        let id = id.to_string();
        self.read(move |s| {
            // PROBE: issue_detail_latency — split the 3 SQLite queries so we
            // know whether the cost is in the issues row read, the label join,
            // or the dependency expansion.
            let q1_started = std::time::Instant::now();
            let mut br = s
                .get_issue(&id)?
                .ok_or_else(|| anyhow::anyhow!("issue {id} not found"))?;
            let q1_ms = q1_started.elapsed().as_millis() as u64;
            // `get_issue` only reads the `issues` table; labels are stored
            // out-of-line and must be loaded separately. Dependencies are also
            // stored out-of-line and back the PM `blocked_by` field.
            let q2_started = std::time::Instant::now();
            let mut by_id = s.get_labels_for_issues(std::slice::from_ref(&id))?;
            let q2_ms = q2_started.elapsed().as_millis() as u64;
            br.labels = by_id.remove(&id).unwrap_or_default();
            let q3_started = std::time::Instant::now();
            br.dependencies = s.get_dependencies_full(&id)?;
            let q3_ms = q3_started.elapsed().as_millis() as u64;
            tracing::info!(
                target: "issue_probe",
                site = "beads_get_issue_queries",
                id = %id,
                q1_get_issue_ms = q1_ms,
                q2_get_labels_ms = q2_ms,
                q3_get_deps_ms = q3_ms,
                n_labels = br.labels.len(),
                n_deps = br.dependencies.len(),
                "per-query timings inside get_issue closure",
            );
            Ok(br_to_pm_issue(br))
        })
        .await
    }

    async fn list_issues(&self, filter: IssueFilter) -> anyhow::Result<Vec<IssueSummary>> {
        self.read(move |s| {
            let mut br_filters = beads_rust::storage::sqlite::ListFilters::default();

            if !filter.labels.is_empty() {
                br_filters.labels = Some(filter.labels.clone());
            }
            if let Some(status) = filter.status.as_deref() {
                let parsed = beads_rust::model::Status::from_str(status)
                    .unwrap_or(beads_rust::model::Status::Open);
                br_filters.statuses = Some(vec![parsed]);
            }
            if let Some(itype) = filter.issue_type.as_deref() {
                let parsed = beads_rust::model::IssueType::from_str(itype)
                    .unwrap_or(beads_rust::model::IssueType::Task);
                br_filters.types = Some(vec![parsed]);
            }
            br_filters.assignee = filter.assignee.clone();
            if let Some(min) = filter.priority_min {
                let max = filter.priority_max.unwrap_or(4);
                let priorities: Vec<beads_rust::model::Priority> =
                    (min..=max).map(beads_rust::model::Priority).collect();
                br_filters.priorities = Some(priorities);
            } else if let Some(max) = filter.priority_max {
                let priorities: Vec<beads_rust::model::Priority> =
                    (0..=max).map(beads_rust::model::Priority).collect();
                br_filters.priorities = Some(priorities);
            }
            br_filters.title_contains = filter.text_search.clone();
            br_filters.include_closed = filter.include_closed || filter.status.is_some();
            let offset = filter.offset.unwrap_or(0);
            br_filters.limit = filter.limit.map(|limit| limit.saturating_add(offset));
            if let Some(since) = filter.since {
                br_filters.updated_after = Some(since);
            }

            let mut issues = s.list_issues(&br_filters)?;
            let allow_tombstones = br_filters
                .statuses
                .as_ref()
                .is_some_and(|statuses| statuses.contains(&beads_rust::model::Status::Tombstone));
            if !allow_tombstones {
                issues.retain(|issue| issue.status != beads_rust::model::Status::Tombstone);
            }
            let ids: Vec<String> = issues.iter().map(|issue| issue.id.clone()).collect();
            let mut labels_by_id = s.get_labels_for_issues(&ids)?;
            for issue in &mut issues {
                issue.labels = labels_by_id.remove(&issue.id).unwrap_or_default();
            }
            let summaries = issues.into_iter().skip(offset).map(br_to_pm_summary);
            Ok(match filter.limit {
                Some(limit) => summaries.take(limit).collect(),
                None => summaries.collect(),
            })
        })
        .await
    }

    async fn create_issue(&self, params: IssueCreate) -> anyhow::Result<String> {
        let actor = self.actor();
        self.write(move |s| {
            validate_create_labels(&params.labels)?;
            if let Some(external_ref) = params.external_ref.as_deref() {
                if let Some(existing) = s.find_by_external_ref(external_ref)? {
                    return Ok(existing.id);
                }
            }

            let now = Utc::now();
            let id = beads_rust::util::generate_id(
                &params.title,
                params.description.as_deref(),
                Some("spur"),
                now,
            );

            let issue_type = params
                .issue_type
                .as_deref()
                .map(|t| {
                    beads_rust::model::IssueType::from_str(t)
                        .unwrap_or(beads_rust::model::IssueType::Task)
                })
                .unwrap_or_default();

            let priority = params
                .priority
                .map(beads_rust::model::Priority)
                .unwrap_or_default();

            let issue = beads_rust::model::Issue {
                id: id.clone(),
                title: params.title,
                description: params.description,
                status: beads_rust::model::Status::Open,
                priority,
                issue_type,
                created_at: now,
                updated_at: now,
                assignee: params.assignee,
                owner: None,
                estimated_minutes: params.estimate_minutes.and_then(|m| i32::try_from(m).ok()),
                due_at: None,
                defer_until: None,
                external_ref: params.external_ref,
                ephemeral: false,
                content_hash: None,
                design: None,
                acceptance_criteria: None,
                notes: None,
                created_by: Some(actor.clone()),
                closed_at: None,
                close_reason: None,
                closed_by_session: None,
                source_system: params.source_system,
                source_repo: params.source_repo,
                deleted_at: None,
                deleted_by: None,
                delete_reason: None,
                original_type: None,
                compaction_level: None,
                compacted_at: None,
                compacted_at_commit: None,
                original_size: None,
                sender: None,
                pinned: false,
                is_template: false,
                labels: Vec::new(),
                dependencies: Vec::new(),
                comments: Vec::new(),
            };
            let labels = params.labels;

            s.create_issue(&issue, &actor)?;

            if !labels.is_empty() {
                s.set_labels(&id, &labels, &actor)?;
            }

            if let Some(parent) = params.parent.as_deref() {
                s.add_dependency(&id, parent, "parent-child", &actor)?;
            }
            for dep in &params.depends_on {
                s.add_dependency(&id, dep, "blocks", &actor)?;
            }

            Ok(id)
        })
        .await
    }

    async fn find_by_external_ref(&self, external_ref: &str) -> anyhow::Result<Option<Issue>> {
        let external_ref = external_ref.to_string();
        self.read(move |s| Ok(s.find_by_external_ref(&external_ref)?.map(br_to_pm_issue)))
            .await
    }

    async fn update_issue(&self, id: &str, update: IssueUpdate) -> anyhow::Result<()> {
        let id = id.to_string();
        let actor = self.actor();
        self.write(move |s| {
            validate_added_labels(&update.add_labels)?;
            let has_field_update = update.status.is_some()
                || update.priority.is_some()
                || update.assignee.is_some()
                || update.body.is_some()
                || update.external_ref.is_some();

            if has_field_update {
                let mut br_update = beads_rust::storage::sqlite::IssueUpdate::default();
                if let Some(status) = update.status.as_deref() {
                    let parsed = beads_rust::model::Status::from_str(status)
                        .unwrap_or(beads_rust::model::Status::Open);
                    br_update.status = Some(parsed);
                }
                if let Some(p) = update.priority {
                    br_update.priority = Some(beads_rust::model::Priority(p));
                }
                if let Some(ref a) = update.assignee {
                    br_update.assignee = if a.is_empty() {
                        Some(None)
                    } else {
                        Some(Some(a.clone()))
                    };
                }
                if let Some(body) = update.body.as_deref() {
                    br_update.description = Some(Some(body.to_string()));
                }
                if let Some(external_ref) = update.external_ref {
                    br_update.external_ref = Some(external_ref);
                }
                s.update_issue(&id, &br_update, &actor)?;
            }

            for label in &update.add_labels {
                s.add_label(&id, label, &actor)?;
            }
            for label in &update.remove_labels {
                s.remove_label(&id, label, &actor)?;
            }

            if let Some(comment) = update.comment.as_deref() {
                s.add_comment(&id, &actor, comment)?;
            }

            Ok(())
        })
        .await
    }

    async fn add_dependency(&self, issue_id: &str, depends_on_id: &str) -> anyhow::Result<()> {
        let issue_id = issue_id.to_string();
        let depends_on_id = depends_on_id.to_string();
        let actor = self.actor();
        self.write(move |s| {
            s.add_dependency(&issue_id, &depends_on_id, "blocks", &actor)?;
            Ok(())
        })
        .await
    }

    async fn poll(&self) -> anyhow::Result<Vec<PmEvent>> {
        self.poll_with_limit(POLL_FETCH_LIMIT).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use beads_rust::model::{Issue as BrIssue, IssueType, Priority, Status};
    use chrono::Utc;
    use tempfile::TempDir;

    use crate::adapter::IssueTracker;
    use crate::beads_crate::adapter::{AdapterConfig, BeadsCrateAdapter};
    use crate::types::{IssueCreate, IssueFilter, IssueUpdate, PmEvent};

    fn minimal_issue(id: &str, title: &str) -> BrIssue {
        let now = Utc::now();
        BrIssue {
            id: id.into(),
            title: title.into(),
            description: None,
            status: Status::Open,
            priority: Priority::MEDIUM,
            issue_type: IssueType::Task,
            created_at: now,
            updated_at: now,
            assignee: None,
            owner: None,
            estimated_minutes: None,
            due_at: None,
            defer_until: None,
            external_ref: None,
            ephemeral: false,
            content_hash: None,
            design: None,
            acceptance_criteria: None,
            notes: None,
            created_by: None,
            closed_at: None,
            close_reason: None,
            closed_by_session: None,
            source_system: None,
            source_repo: None,
            deleted_at: None,
            deleted_by: None,
            delete_reason: None,
            original_type: None,
            compaction_level: None,
            compacted_at: None,
            compacted_at_commit: None,
            original_size: None,
            sender: None,
            pinned: false,
            is_template: false,
            labels: Vec::new(),
            dependencies: Vec::new(),
            comments: Vec::new(),
        }
    }

    fn event_ids(events: &[PmEvent]) -> HashSet<String> {
        events
            .iter()
            .map(|event| match event {
                PmEvent::IssueCreated(summary) | PmEvent::IssueUpdated(summary) => {
                    summary.id.clone()
                }
            })
            .collect()
    }

    fn issue_event_is_updated(event: &PmEvent) -> bool {
        matches!(event, PmEvent::IssueUpdated(_))
    }

    fn issue_event_is_created(event: &PmEvent) -> bool {
        matches!(event, PmEvent::IssueCreated(_))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_issue_returns_seeded_issue() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();
        let seed_issue = minimal_issue("bd-test-get-issue", "seeded issue");
        let expected_id = seed_issue.id.clone();
        let expected_title = seed_issue.title.clone();

        adapter
            .write(move |s| s.create_issue(&seed_issue, "test").map_err(Into::into))
            .await
            .unwrap();

        let issue = adapter.get_issue(&expected_id).await.unwrap();

        assert_eq!(issue.id, expected_id);
        assert_eq!(issue.title, expected_title);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_issue_round_trips_via_get_issue() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();

        let id = adapter
            .create_issue(IssueCreate {
                title: "Hello".into(),
                description: Some("Body".into()),
                priority: Some(1),
                labels: vec!["lbl-a".into(), "lbl-b".into()],
                ..Default::default()
            })
            .await
            .unwrap();

        assert!(!id.is_empty());

        let fetched = adapter.get_issue(&id).await.unwrap();
        assert_eq!(fetched.title, "Hello");
        assert_eq!(fetched.body, "Body");
        assert_eq!(fetched.priority, Some(1));
        assert_eq!(
            fetched.labels,
            vec!["lbl-a".to_string(), "lbl-b".to_string()]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_issue_round_trips_provenance_fields() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();

        let id = adapter
            .create_issue(IssueCreate {
                title: "Imported issue".into(),
                source_system: Some("github".into()),
                source_repo: Some("getspur/spur".into()),
                external_ref: Some("github/I_kwDOExample123".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        let fetched = adapter.get_issue(&id).await.unwrap();
        assert_eq!(fetched.source_system.as_deref(), Some("github"));
        assert_eq!(fetched.source_repo.as_deref(), Some("getspur/spur"));
        assert_eq!(
            fetched.external_ref.as_deref(),
            Some("github/I_kwDOExample123")
        );

        let found = adapter
            .find_by_external_ref("github/I_kwDOExample123")
            .await
            .unwrap()
            .expect("find by external ref");
        assert_eq!(found.id, id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_issue_is_idempotent_for_existing_external_ref() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();

        let first = adapter
            .create_issue(IssueCreate {
                title: "Imported issue".into(),
                source_system: Some("github".into()),
                source_repo: Some("getspur/spur".into()),
                external_ref: Some("github/I_kwDOExample123".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        let second = adapter
            .create_issue(IssueCreate {
                title: "Imported issue renamed upstream".into(),
                source_system: Some("github".into()),
                source_repo: Some("getspur/spur".into()),
                external_ref: Some("github/I_kwDOExample123".into()),
                ..Default::default()
            })
            .await
            .unwrap();

        assert_eq!(second, first);

        let issues = adapter
            .list_issues(IssueFilter {
                include_closed: true,
                ..Default::default()
            })
            .await
            .unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].title, "Imported issue");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_issue_can_set_and_clear_external_ref() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();

        let id = adapter
            .create_issue(IssueCreate {
                title: "Local issue".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        adapter
            .update_issue(
                &id,
                IssueUpdate {
                    external_ref: Some(Some("github/I_kwDOExample123".into())),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let with_ref = adapter.get_issue(&id).await.unwrap();
        assert_eq!(
            with_ref.external_ref.as_deref(),
            Some("github/I_kwDOExample123")
        );

        adapter
            .update_issue(
                &id,
                IssueUpdate {
                    external_ref: Some(None),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let without_ref = adapter.get_issue(&id).await.unwrap();
        assert_eq!(without_ref.external_ref, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_issue_changes_status_and_labels() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();

        let id = adapter
            .create_issue(IssueCreate {
                title: "X".into(),
                labels: vec!["keep".into(), "drop".into()],
                ..Default::default()
            })
            .await
            .unwrap();

        adapter
            .update_issue(
                &id,
                IssueUpdate {
                    status: Some("closed".into()),
                    priority: Some(0),
                    add_labels: vec!["new".into()],
                    remove_labels: vec!["drop".into()],
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let after = adapter.get_issue(&id).await.unwrap();
        assert_eq!(after.status, "closed");
        assert_eq!(after.priority, Some(0));
        let mut labels = after.labels;
        labels.sort();
        assert_eq!(labels, vec!["keep".to_string(), "new".to_string()]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn add_dependency_links_two_issues() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();

        let parent = adapter
            .create_issue(IssueCreate {
                title: "P".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let child = adapter
            .create_issue(IssueCreate {
                title: "C".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        adapter.add_dependency(&child, &parent).await.unwrap();

        // Verify the edge through the storage's own dependency reader.
        let parent_for_check = parent.clone();
        let child_for_check = child.clone();
        let deps = adapter
            .read(move |s| Ok(s.get_dependencies(&child_for_check)?))
            .await
            .unwrap();
        assert!(
            deps.contains(&parent_for_check),
            "expected {parent_for_check} in deps, got {deps:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_emits_created_then_quiesces() {
        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();

        adapter
            .create_issue(IssueCreate {
                title: "X".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        let first = adapter.poll().await.unwrap();
        assert_eq!(first.len(), 1);
        assert!(matches!(first[0], PmEvent::IssueCreated(_)));

        let second = adapter.poll().await.unwrap();
        assert!(
            second.is_empty(),
            "second poll should be empty, got {:?}",
            second
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_does_not_advance_cursor_on_limit_saturation() {
        let dir = TempDir::new().unwrap();
        let cursor_path = dir.path().join(".spur-test-cursor");
        let config = AdapterConfig {
            cursor_path: Some(cursor_path),
            ..AdapterConfig::default()
        };
        let adapter = BeadsCrateAdapter::open(dir.path(), config).await.unwrap();

        let initial = adapter
            .create_issue(IssueCreate {
                title: "Initial".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let prime = adapter.poll_with_limit(10).await.unwrap();
        assert_eq!(prime.len(), 1);
        adapter
            .update_issue(
                &initial,
                IssueUpdate {
                    status: Some("closed".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let cursor_before = adapter
            .cursor
            .lock()
            .await
            .clone()
            .expect("prime poll should set cursor");

        let mut new_ids = HashSet::new();
        for i in 0..5 {
            let id = adapter
                .create_issue(IssueCreate {
                    title: format!("Issue {i}"),
                    ..Default::default()
                })
                .await
                .unwrap();
            new_ids.insert(id);
        }

        let saturated_events = adapter.poll_with_limit(3).await.unwrap();
        assert_eq!(saturated_events.len(), 3);
        assert!(saturated_events.iter().all(issue_event_is_updated));
        let cursor_after = adapter
            .cursor
            .lock()
            .await
            .clone()
            .expect("saturated poll with prior cursor should preserve it");
        assert_eq!(cursor_after.ts, cursor_before.ts);
        assert_eq!(cursor_after.ids_at_boundary, cursor_before.ids_at_boundary);

        let saturated_ids = event_ids(&saturated_events);
        for id in &saturated_ids {
            adapter
                .update_issue(
                    id,
                    IssueUpdate {
                        status: Some("closed".into()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }

        let remaining_events = adapter.poll_with_limit(10).await.unwrap();
        assert!(remaining_events.iter().all(issue_event_is_updated));
        let remaining_ids = event_ids(&remaining_events);
        let expected_remaining: HashSet<String> =
            new_ids.difference(&saturated_ids).cloned().collect();
        assert_eq!(remaining_ids, expected_remaining);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn saturated_first_poll_keeps_cursor_unset_until_backlog_drains() {
        let dir = TempDir::new().unwrap();
        let cursor_path = dir.path().join(".spur-test-cursor");
        let config = AdapterConfig {
            cursor_path: Some(cursor_path.clone()),
            ..AdapterConfig::default()
        };
        let adapter = BeadsCrateAdapter::open(dir.path(), config).await.unwrap();

        let mut all_ids = HashSet::new();
        for i in 0..5 {
            let id = adapter
                .create_issue(IssueCreate {
                    title: format!("Issue {i}"),
                    ..Default::default()
                })
                .await
                .unwrap();
            all_ids.insert(id);
        }

        let first_events = adapter.poll_with_limit(3).await.unwrap();
        assert_eq!(first_events.len(), 3);
        assert!(first_events.iter().all(issue_event_is_created));
        assert!(
            adapter.cursor.lock().await.is_none(),
            "first saturated poll should leave in-memory cursor unset"
        );
        assert!(
            !cursor_path.exists(),
            "first saturated poll should not write a cursor file"
        );

        let first_ids = event_ids(&first_events);
        for id in &first_ids {
            adapter
                .update_issue(
                    id,
                    IssueUpdate {
                        status: Some("closed".into()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        }

        let second_events = adapter.poll_with_limit(10).await.unwrap();
        assert!(second_events.iter().all(issue_event_is_created));
        let second_ids = event_ids(&second_events);
        let expected_second: HashSet<String> = all_ids.difference(&first_ids).cloned().collect();
        assert_eq!(second_ids, expected_second);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_issues_returns_seeded_data() {
        use crate::types::IssueFilter;

        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();

        adapter
            .batch(|s| {
                for i in 0..3 {
                    let issue = minimal_issue(&format!("bd-list-{i}"), &format!("issue {i}"));
                    s.create_issue(&issue, "test")?;
                }
                Ok(())
            })
            .await
            .unwrap();

        let summaries = adapter.list_issues(IssueFilter::default()).await.unwrap();
        assert_eq!(summaries.len(), 3);
        let ids: Vec<&str> = summaries.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"bd-list-0"));
        assert!(ids.contains(&"bd-list-1"));
        assert!(ids.contains(&"bd-list-2"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_issues_include_closed_excludes_tombstones() {
        use crate::types::IssueFilter;

        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();

        let open_id = adapter
            .create_issue(IssueCreate {
                title: "Open issue".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let closed_id = adapter
            .create_issue(IssueCreate {
                title: "Closed issue".into(),
                ..Default::default()
            })
            .await
            .unwrap();
        let deleted_id = adapter
            .create_issue(IssueCreate {
                title: "Deleted issue".into(),
                ..Default::default()
            })
            .await
            .unwrap();

        adapter
            .update_issue(
                &closed_id,
                IssueUpdate {
                    status: Some("closed".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let deleted_id_for_write = deleted_id.clone();
        adapter
            .write(move |s| {
                s.delete_issue(&deleted_id_for_write, "test", "deleted", None)?;
                Ok(())
            })
            .await
            .unwrap();

        let summaries = adapter
            .list_issues(IssueFilter {
                include_closed: true,
                ..Default::default()
            })
            .await
            .unwrap();

        let ids: HashSet<&str> = summaries.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(open_id.as_str()));
        assert!(ids.contains(closed_id.as_str()));
        assert!(
            !ids.contains(deleted_id.as_str()),
            "include_closed should not return tombstones"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_issues_applies_offset_after_fetching_limit_window() {
        use crate::types::IssueFilter;

        let dir = TempDir::new().unwrap();
        let adapter = BeadsCrateAdapter::open(dir.path(), AdapterConfig::default())
            .await
            .unwrap();

        adapter
            .batch(|s| {
                for i in 0..5 {
                    let mut issue = minimal_issue(&format!("bd-offset-{i}"), &format!("issue {i}"));
                    issue.priority = Priority(i);
                    s.create_issue(&issue, "test")?;
                }
                Ok(())
            })
            .await
            .unwrap();

        let summaries = adapter
            .list_issues(IssueFilter {
                limit: Some(2),
                offset: Some(2),
                include_closed: true,
                ..Default::default()
            })
            .await
            .unwrap();

        let ids: Vec<&str> = summaries.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["bd-offset-2", "bd-offset-3"]);
    }
}
