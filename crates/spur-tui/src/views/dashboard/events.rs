impl DashboardView {
    fn on_brain_event(&mut self, body: &SpurEventBody) {
        match body {
            SpurEventBody::BrainConnectStarted { brain } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[brain:{}]", brain),
                    message: "Connecting…".to_string(),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::BrainConnected { brain } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[brain:{}]", brain),
                    message: "Connected".to_string(),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::BrainConnectFailed { brain, reason } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[brain:{}]", brain),
                    message: format!("Connect failed: {}", truncate_display(reason, 120)),
                    kind: LogEntryKind::Error,
                });
            }

            SpurEventBody::BrainSpawned { agent, session: _ } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[brain:{}]", agent),
                    message: "Brain agent spawned".to_string(),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::BrainRetired { session, reason } => {
                // Pair the earlier "Brain agent spawned" entry with an
                // explicit retirement line so the activity log does not
                // show a dangling spawn after `/clear` or session switch.
                let prefix = Self::prefix_for_session(&session.0);
                let reason_label = match reason {
                    spur_acp::domain::events::BrainRetireReason::UserClear => "cleared",
                    spur_acp::domain::events::BrainRetireReason::ResumeSwitch => "switched",
                    spur_acp::domain::events::BrainRetireReason::Shutdown => "shutdown",
                    _ => "retired",
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: format!("Brain session retired ({})", reason_label),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::BrainFailover { from, to } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[spur]".to_string(),
                    message: format!("Brain failover: {} -> {}", from, to),
                    kind: LogEntryKind::Error,
                });
            }

            SpurEventBody::BrainError { session, message } => {
                let prefix = Self::prefix_for_session(&session.0);
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: format!("Brain error: {}", message),
                    kind: LogEntryKind::Error,
                });
            }

            SpurEventBody::BrainReconnecting {
                session,
                brain_name,
                reason,
            } => {
                let prefix = Self::prefix_for_session(&session.0);
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: format!("Brain '{}' reconnecting: {}", brain_name, reason),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::BrainReconnected {
                session,
                brain_name,
                outcome,
            } => {
                let prefix = Self::prefix_for_session(&session.0);
                let (message, kind) = match outcome {
                    spur_acp::LoadOutcome::Restored => (
                        format!(
                            "Brain '{}' reconnected (state restored; your last prompt was dropped — retype)",
                            brain_name
                        ),
                        LogEntryKind::Info,
                    ),
                    spur_acp::LoadOutcome::FellBackToNew { reason } => (
                        format!(
                            "Brain '{}' reconnected — started FRESH ({}); retype to continue",
                            brain_name, reason
                        ),
                        LogEntryKind::Error,
                    ),
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message,
                    kind,
                });
            }

            SpurEventBody::BrainReconnectFailed {
                session,
                brain_name,
                reason,
            } => {
                let prefix = Self::prefix_for_session(&session.0);
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: format!("Brain '{}' reconnect FAILED: {}", brain_name, reason),
                    kind: LogEntryKind::Error,
                });
            }
            _ => {}
        }
    }

    fn on_worker_event(&mut self, body: &SpurEventBody) {
        match body {
            SpurEventBody::WorkerSpawned {
                agent,
                session: _,
                worktree: _,
            } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[worker:{}]", agent),
                    message: "Worker agent spawned".to_string(),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::WorkerProgress {
                executor_id,
                name,
                pct,
                ..
            } => {
                let msg = match pct {
                    Some(p) => format!("{} ({}%)", name, p),
                    None => name.clone(),
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: Self::prefix_for_session(executor_id),
                    message: msg,
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::WorkerReportProgress {
                delegation_id,
                message,
                percent,
            } => {
                let msg = match percent {
                    Some(p) => format!("{} ({}%)", message, p),
                    None => message.clone(),
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[delegation:{}]", truncate_display(delegation_id, 12)),
                    message: msg,
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::WorkerFileTouched {
                executor_id,
                path,
                kind,
                ..
            } => {
                let verb = match kind {
                    spur_acp::FileTouchKind::Read => "read",
                    spur_acp::FileTouchKind::Write => "wrote",
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: Self::prefix_for_session(executor_id),
                    message: format!("{} {}", verb, path.display()),
                    kind: LogEntryKind::Act,
                });
            }

            SpurEventBody::OrphanReaped {
                agent_name,
                pgid,
                age_secs,
            } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[orphan:{}]", agent_name),
                    message: format!("Reaped orphan (pgid {pgid}, age {age_secs}s)"),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::DispatchLeaseExpired {
                plan_id,
                task_id,
                issue_id,
                delegation_id,
                expired_at,
                age_secs,
            } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[plan]".to_string(),
                    message: format!(
                        "Dispatch lease expired for {plan_id}/{task_id} ({issue_id}, {delegation_id}, expired at {expired_at}, age {age_secs}s)"
                    ),
                    kind: LogEntryKind::Error,
                });
            }
            _ => {}
        }
    }

    fn on_delegation_event(&mut self, body: &SpurEventBody) {
        match body {
            SpurEventBody::DelegationRequested {
                from: _,
                to_agent,
                task,
                request_id: _,
                delegation_plan: _,
                issue_id: _,
            } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[brain]".to_string(),
                    message: format!("Delegating to {}: {}", to_agent, task),
                    kind: LogEntryKind::Delegate,
                });
            }

            SpurEventBody::DelegationCompleted {
                worker_session,
                status,
            } => {
                let prefix = Self::prefix_for_session(&worker_session.0);
                let (msg, kind) = match status {
                    DelegationStatus::Success => (
                        "Delegation completed successfully".to_string(),
                        LogEntryKind::Complete,
                    ),
                    DelegationStatus::Failed { error } => {
                        (format!("Delegation failed: {}", error), LogEntryKind::Error)
                    }
                    DelegationStatus::Conflict { files } => (
                        format!("Delegation conflict in {} files", files.len()),
                        LogEntryKind::Error,
                    ),
                    DelegationStatus::Timeout => {
                        ("Delegation timed out".to_string(), LogEntryKind::Error)
                    }
                    DelegationStatus::Rejected { reason } => (
                        format!("Delegation rejected: {}", reason),
                        LogEntryKind::Error,
                    ),
                    DelegationStatus::Modified { reviewer_note } => (
                        format!("Delegation modified: {}", reviewer_note),
                        LogEntryKind::Complete,
                    ),
                    DelegationStatus::TimedOut {
                        waited_for,
                        fallback,
                    } => (
                        format!(
                            "Delegation review timed out after {}s (fallback: {:?})",
                            waited_for.as_secs(),
                            fallback
                        ),
                        LogEntryKind::Error,
                    ),
                    DelegationStatus::Cancelled { reason } => (
                        format!("Delegation cancelled: {}", reason),
                        LogEntryKind::Error,
                    ),
                    _ => {
                        tracing::warn!("unknown DelegationStatus variant in dashboard activity log — update needed");
                        ("Delegation completed".to_string(), LogEntryKind::Error)
                    }
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: msg,
                    kind,
                });
            }
            _ => {}
        }
    }

    fn on_issue_event(&mut self, body: &SpurEventBody) {
        match body {
            SpurEventBody::IssueReceived { source, id } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[pm]".to_string(),
                    message: format!("Issue received from {}: {}", source, id),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::PrCreated { url } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[spur]".to_string(),
                    message: format!("PR created: {}", url),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::IssueUpdated {
                source,
                id,
                status,
                assignee,
            } => {
                let mut updated_issue = false;
                if let Some(issue) = self.tracked_issues.iter_mut().find(|i| i.id == *id) {
                    if let Some(ref s) = status {
                        issue.status = s.clone();
                    }
                    if let Some(a) = assignee {
                        issue.assignee = Some(a.clone());
                    }
                    updated_issue = true;
                }
                if updated_issue {
                    self.refresh_mention_issues();
                }
                let status_suffix = status
                    .as_ref()
                    .map(|s| format!(": {}", s))
                    .unwrap_or_default();
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[pm]".into(),
                    message: format!("Issue {} ({}) updated{}", id, source, status_suffix),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::IssueCreated { issue } => {
                let created_issue = spur_pm::IssueSummary {
                    id: issue.id.clone(),
                    source: match issue.source.as_str() {
                        "github" => spur_pm::PmSource::GitHub,
                        "linear" => spur_pm::PmSource::Linear,
                        "plane" => spur_pm::PmSource::Plane,
                        _ => spur_pm::PmSource::Beads,
                    },
                    title: issue.title.clone(),
                    status: issue.status.clone(),
                    labels: issue.labels.clone(),
                    url: String::new(),
                    priority: issue.priority,
                    issue_type: issue.issue_type.clone(),
                    assignee: issue.assignee.clone(),
                    description: issue.description.clone(),
                };
                if let Some(existing) = self
                    .tracked_issues
                    .iter_mut()
                    .find(|existing| existing.id == created_issue.id)
                {
                    *existing = created_issue;
                } else {
                    self.tracked_issues.push(created_issue);
                }
                self.tracked_issues
                    .sort_by(|a, b| a.priority.unwrap_or(99).cmp(&b.priority.unwrap_or(99)));
                self.refresh_mention_issues();
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[pm]".into(),
                    message: format!("Issue {} ({}) created", issue.id, issue.source),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::IssuesLoaded { issues } => {
                let mut loaded_issues = issues
                    .iter()
                    .map(|i| spur_pm::IssueSummary {
                        id: i.id.clone(),
                        source: match i.source.as_str() {
                            "github" => spur_pm::PmSource::GitHub,
                            "linear" => spur_pm::PmSource::Linear,
                            "plane" => spur_pm::PmSource::Plane,
                            _ => spur_pm::PmSource::Beads,
                        },
                        title: i.title.clone(),
                        status: i.status.clone(),
                        labels: i.labels.clone(),
                        url: String::new(),
                        priority: i.priority,
                        issue_type: i.issue_type.clone(),
                        assignee: i.assignee.clone(),
                        description: i.description.clone(),
                    })
                    .collect::<Vec<_>>();
                // Sort by priority ascending (critical first)
                loaded_issues
                    .sort_by(|a, b| a.priority.unwrap_or(99).cmp(&b.priority.unwrap_or(99)));
                self.set_issue_snapshot(loaded_issues);
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[pm]".into(),
                    message: format!("{} issues loaded", self.tracked_issues.len()),
                    kind: LogEntryKind::Info,
                });
            }
            _ => {}
        }
    }

    fn on_session_signal(&mut self, body: &SpurEventBody) {
        match body {
            SpurEventBody::AgentNotification {
                session,
                notification,
            } => {
                let prefix = Self::prefix_for_session(&session.0);
                match &notification.update {
                    spur_acp::SessionUpdate::AgentThoughtChunk(chunk)
                    | spur_acp::SessionUpdate::AgentMessageChunk(chunk) => {
                        if let spur_acp::ContentBlock::Text(tc) = &chunk.content {
                            let trimmed = tc.text.trim();
                            if !trimmed.is_empty() {
                                let entry = self
                                    .text_batch
                                    .entry(session.0.clone())
                                    .or_insert_with(|| (String::new(), Instant::now()));
                                entry.0.push_str(trimmed);
                                if entry.0.len() > 200 {
                                    let mut start = entry.0.len() - 200;
                                    while !entry.0.is_char_boundary(start) {
                                        start += 1;
                                    }
                                    entry.0 = entry.0[start..].to_string();
                                }
                                entry.1 = Instant::now();
                            }
                        }
                    }
                    spur_acp::SessionUpdate::ToolCall(tc) => {
                        self.activity_log.push(LogEntry {
                            timestamp: Self::now_stamp(),
                            prefix,
                            message: format!("\u{1f527} Tool: {}", tc.title),
                            kind: LogEntryKind::Act,
                        });
                    }
                    spur_acp::SessionUpdate::ToolCallUpdate(_) => {
                        // Not logged in dashboard (condensed view)
                    }
                    _ => {
                        // Other variants — no agent-state mutation needed; lineage handles it
                    }
                }
            }

            SpurEventBody::SessionCompleted { session, success } => {
                let prefix = Self::prefix_for_session(&session.0);
                let msg = if *success {
                    "Session completed successfully"
                } else {
                    "Session failed"
                };
                let kind = if *success {
                    LogEntryKind::Complete
                } else {
                    LogEntryKind::Error
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: msg.to_string(),
                    kind,
                });
            }

            SpurEventBody::RateLimitDetected { agent, retry_after } => {
                let msg = match retry_after {
                    Some(d) => format!("Rate limited (retry after {}s)", d.as_secs()),
                    None => "Rate limited".to_string(),
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: format!("[{}]", agent),
                    message: msg,
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::CostUpdate { .. } => {
                // Cost is now read from lineage.nodes().current_attempt().cost_usd
            }

            SpurEventBody::ConflictDetected { files } => {
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[spur]".to_string(),
                    message: format!(
                        "Conflict detected in {} file(s): {}",
                        files.len(),
                        files
                            .iter()
                            .map(|f| f.display().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    kind: LogEntryKind::Info,
                });
            }

            SpurEventBody::TurnComplete { session } => {
                let prefix = Self::prefix_for_session(&session.0);
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix,
                    message: "Turn complete".to_string(),
                    kind: LogEntryKind::Info,
                });
            }
            _ => {}
        }
    }

    fn on_plan_event(&mut self, body: &SpurEventBody) {
        match body {
            SpurEventBody::GraphAlertsSummary {
                total,
                critical,
                warning,
                details,
            } => {
                self.alert_summary = Some((*total, *critical, *warning));
                for msg in details.iter().take(5) {
                    self.activity_log.push(LogEntry {
                        timestamp: Self::now_stamp(),
                        prefix: "[graph]".into(),
                        message: msg.clone(),
                        kind: if *critical > 0 {
                            LogEntryKind::Error
                        } else {
                            LogEntryKind::Info
                        },
                    });
                }
            }

            SpurEventBody::PlanTaskReviewed {
                plan_id: _,
                task_id,
                task_name,
                decision,
                feedback,
                attempt,
                max_attempts,
            } => {
                let (icon, label, kind) = match decision.as_str() {
                    "approve" => ("✓", "approved", LogEntryKind::Complete),
                    "reject" => ("✗", "rejected", LogEntryKind::Error),
                    "request_changes" => ("↻", "requested changes on", LogEntryKind::Act),
                    _ => ("?", "reviewed", LogEntryKind::Info),
                };
                let display = task_name
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(task_id);
                let attempts_suffix = if *max_attempts > 0 {
                    format!(" (attempt {attempt}/{max_attempts})")
                } else {
                    format!(" (attempt {attempt})")
                };
                let fb_suffix = feedback
                    .as_ref()
                    .map(|f| format!(": \"{}\"", truncate_display(f, 60)))
                    .unwrap_or_default();
                // Distinct entry when attempt budget is exhausted by a reject.
                let exhausted =
                    decision == "reject" && *max_attempts > 0 && *attempt >= *max_attempts;
                let message = if exhausted {
                    format!(
                        "✗ Task \"{display}\" failed — max attempts ({max_attempts}) reached{fb_suffix}"
                    )
                } else {
                    format!("{icon} Brain {label} \"{display}\"{attempts_suffix}{fb_suffix}")
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[plan]".to_string(),
                    message,
                    kind,
                });
            }

            SpurEventBody::PlanTaskIterating {
                plan_id: _,
                task_id,
                task_name,
                attempt,
                max_attempts,
                delegation_id: _,
            } => {
                let display = task_name
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or(task_id);
                let attempts_suffix = if *max_attempts > 0 {
                    format!("{attempt}/{max_attempts}")
                } else {
                    format!("{attempt}")
                };
                let final_hint = if *max_attempts > 0 && *attempt >= *max_attempts {
                    " — final"
                } else {
                    ""
                };
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[plan]".to_string(),
                    message: format!(
                        "↻ Re-dispatched \"{display}\" (attempt {attempts_suffix}{final_hint})"
                    ),
                    kind: LogEntryKind::Act,
                });
            }

            SpurEventBody::PlanPendingSweep {
                plan_id,
                epic_id,
                action,
                child_count,
                age_secs,
                reason,
            } => {
                let plan = plan_id.as_deref().unwrap_or("unknown");
                self.activity_log.push(LogEntry {
                    timestamp: Self::now_stamp(),
                    prefix: "[plan]".to_string(),
                    message: format!(
                        "Pending sweep {action} epic {epic_id} for {plan} ({child_count} children, age {age_secs}s): {reason}"
                    ),
                    kind: LogEntryKind::Info,
                });
            }
            _ => {}
        }
    }

}
