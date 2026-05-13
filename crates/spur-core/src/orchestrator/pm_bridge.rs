use spur_acp::{GraphEdgeEvent, GraphNodeEvent, SpurEventBody};

/// Convert spur_pm::IssueSummary to the spur_acp mirror type for event bus transmission.
pub(super) fn to_summary_event(
    issue: &spur_pm::IssueSummary,
    source: &str,
) -> spur_acp::domain::events::IssueSummaryEvent {
    spur_acp::domain::events::IssueSummaryEvent {
        id: issue.id.clone(),
        source: source.into(),
        title: issue.title.clone(),
        status: issue.status.clone(),
        labels: issue.labels.clone(),
        priority: issue.priority,
        issue_type: issue.issue_type.clone(),
        assignee: issue.assignee.clone(),
        description: issue.description.clone(),
    }
}

/// Emit a `GraphAlertsSummary` event from a triage report's alert list.
pub(super) fn emit_alerts_from_report(
    report: &spur_pm::graph::TriageReport,
    funnel: &crate::event_funnel::FunnelHandle,
) {
    let alerts = &report.triage.alerts;
    let critical = alerts
        .iter()
        .filter(|a| a.severity.as_deref() == Some("critical"))
        .count();
    let warning = alerts
        .iter()
        .filter(|a| a.severity.as_deref() == Some("warning"))
        .count();
    let details: Vec<String> = alerts
        .iter()
        .take(5)
        .filter_map(|a| a.message.clone())
        .collect();
    funnel.emit(SpurEventBody::GraphAlertsSummary {
        total: alerts.len(),
        critical,
        warning,
        details,
    });
}

/// Build a brain-prompt summary from a triage report.
pub(super) fn build_graph_prompt_summary(report: &spur_pm::graph::TriageReport) -> Option<String> {
    let qr = &report.triage.quick_ref;
    let health = &report.triage.project_health;
    let mut lines = vec![
        "## Project Graph Intelligence".to_string(),
        String::new(),
        format!(
            "Project: {} open, {} actionable, {} blocked, {} in progress.",
            qr.open_count, qr.actionable_count, qr.blocked_count, qr.in_progress_count,
        ),
    ];
    if let Some(top) = qr.top_picks.first() {
        lines.push(format!(
            "Top recommendation: {} (score {:.2}) — \"{}\"",
            top.id, top.score, top.title,
        ));
    }
    if health.graph.has_cycles {
        lines.push(format!(
            "Warning: {} cycles detected in dependency graph.",
            health.graph.cycle_count,
        ));
    }
    if !report.triage.quick_wins.is_empty() {
        let ids: Vec<_> = report
            .triage
            .quick_wins
            .iter()
            .take(3)
            .map(|q| q.id.as_str())
            .collect();
        lines.push(format!("Quick wins: {}", ids.join(", ")));
    }
    lines.push(String::new());
    lines.push(
        "Use `graph_triage` for full analysis. \
         Use `graph_plan` for parallel execution tracks."
            .to_string(),
    );
    Some(lines.join("\n"))
}

/// Parallel-fetch issues + graph triage, emit `IssuesLoaded` +
/// `GraphAlertsSummary` events via `tokio::join!`. When `for_prompt` is
/// true, also returns a graph summary string for brain prompt enrichment.
///
/// Replaces the previous sequential `list_issues` → `emit_graph_alerts`
/// pattern at all 4 call sites. Wall-time: `max(T_br, T_bv)` instead of
/// `T_br + T_bv`.
pub(super) async fn refresh_pm_state(
    pm: &spur_pm::PmService,
    funnel: &crate::event_funnel::FunnelHandle,
    limit: Option<usize>,
    for_prompt: bool,
) -> Option<String> {
    let issues_fut = pm.list_issues(spur_pm::IssueFilter {
        status: Some("open".into()),
        limit,
        ..Default::default()
    });

    let triage_fut = async {
        match pm.analyzer() {
            Some(bv) => bv.triage(None).await.ok(),
            None => None,
        }
    };

    let (issues_result, triage_opt) = tokio::join!(issues_fut, triage_fut);

    // Emit issues.
    match issues_result {
        Ok(issues) => {
            let event_issues: Vec<_> = issues
                .iter()
                .map(|i| to_summary_event(i, pm.source_str()))
                .collect();
            tracing::info!(
                count = issues.len(),
                "Loaded open issues from {}",
                pm.source_str()
            );
            funnel.emit(SpurEventBody::IssuesLoaded {
                issues: event_issues,
            });
        }
        Err(e) => {
            // Surface to the TUI so the empty list isn't indistinguishable from
            // a genuinely empty backlog. Without this emit, parse failures (e.g.
            // a corrupt `.beads/issues.jsonl` from a bad git merge) leave the
            // view stuck on "No issues loaded" with no signal of the real cause.
            let error = e.to_string();
            tracing::warn!("Failed to load issues: {error}");
            funnel.emit(SpurEventBody::IssueCommandError {
                operation: "list_issues".into(),
                error,
                id: None,
            });
        }
    }

    // Emit alerts + optionally build prompt summary.
    if let Some(report) = triage_opt {
        emit_alerts_from_report(&report, funnel);
        if for_prompt {
            return build_graph_prompt_summary(&report);
        }
    }
    None
}

/// Convert spur_pm::Issue to the spur_acp mirror type for event bus transmission.
pub(super) fn issue_to_detail_event(
    issue: &spur_pm::Issue,
    comments: Vec<spur_pm::Comment>,
) -> spur_acp::IssueDetailEvent {
    spur_acp::IssueDetailEvent {
        id: issue.id.clone(),
        source: issue.source.to_string(),
        title: issue.title.clone(),
        body: issue.body.clone(),
        status: issue.status.clone(),
        labels: issue.labels.clone(),
        assignee: issue.assignee.clone(),
        url: issue.url.clone(),
        priority: issue.priority,
        issue_type: issue.issue_type.clone(),
        blocked_by: issue.blocked_by.clone(),
        due_at: issue.due_at,
        comments,
        created_at: issue.created_at,
        updated_at: issue.updated_at,
    }
}

pub(super) fn graph_node_to_event(node: &spur_pm::graph::GraphNode) -> spur_acp::GraphNodeEvent {
    spur_acp::GraphNodeEvent {
        id: node.id.clone(),
        title: node.title.clone(),
        status: node.status.clone(),
        priority: node.priority,
        labels: node.labels.clone(),
        pagerank: node.pagerank,
    }
}

pub(super) fn graph_edge_to_event(edge: &spur_pm::graph::GraphEdge) -> spur_acp::GraphEdgeEvent {
    spur_acp::GraphEdgeEvent {
        from: edge.from.clone(),
        to: edge.to.clone(),
        edge_type: edge.edge_type.clone(),
    }
}

pub(super) fn dependency_graph_to_event_parts(
    graph: spur_pm::graph::DependencyGraph,
) -> (Vec<GraphNodeEvent>, Vec<GraphEdgeEvent>) {
    let Some(adjacency) = graph.adjacency else {
        return (Vec::new(), Vec::new());
    };

    let nodes = adjacency.nodes.iter().map(graph_node_to_event).collect();
    let edges = adjacency
        .edges
        .unwrap_or_default()
        .iter()
        .map(graph_edge_to_event)
        .collect();
    (nodes, edges)
}

#[async_trait::async_trait]
pub(super) trait IssueGraphPm {
    fn analyzer_available(&self) -> bool;

    async fn issue_subgraph_json(
        &self,
        id: &str,
    ) -> anyhow::Result<spur_pm::graph::DependencyGraph>;
}

#[async_trait::async_trait]
impl IssueGraphPm for spur_pm::PmService {
    fn analyzer_available(&self) -> bool {
        self.issue_graph_available()
    }

    async fn issue_subgraph_json(
        &self,
        id: &str,
    ) -> anyhow::Result<spur_pm::graph::DependencyGraph> {
        spur_pm::PmService::issue_subgraph_json(self, id).await
    }
}

pub(super) async fn handle_get_issue_graph<P: IssueGraphPm + ?Sized>(
    pm: Option<&P>,
    funnel: &crate::event_funnel::FunnelHandle,
    id: String,
) {
    let Some(pm) = pm else {
        funnel.emit(SpurEventBody::IssueCommandError {
            operation: "GetIssueGraph".into(),
            error: "No issue tracker configured".into(),
            id: Some(id),
        });
        return;
    };

    if !pm.analyzer_available() {
        funnel.emit(SpurEventBody::IssueCommandError {
            operation: "GetIssueGraph".into(),
            error: "Issue graph unavailable for configured issue tracker".into(),
            id: Some(id),
        });
        return;
    }

    match pm.issue_subgraph_json(&id).await {
        Ok(graph) => {
            let (nodes, edges) = dependency_graph_to_event_parts(graph);
            funnel.emit(SpurEventBody::IssueSubgraphLoaded {
                requested_id: id,
                nodes,
                edges,
            });
        }
        Err(e) => {
            funnel.emit(SpurEventBody::IssueCommandError {
                operation: "GetIssueGraph".into(),
                error: e.to_string(),
                id: Some(id),
            });
        }
    }
}

#[cfg(test)]
mod issue_graph_handler_tests {
    use super::{handle_get_issue_graph, IssueGraphPm};
    use async_trait::async_trait;
    use spur_acp::SpurEventBody;
    use spur_pm::graph::{AdjacencyData, DependencyGraph, GraphEdge, GraphNode};
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::sync::mpsc::UnboundedReceiver;

    struct FakePmService {
        analyzer_available: bool,
        result: Mutex<Option<Result<DependencyGraph, String>>>,
        requested_ids: Mutex<Vec<String>>,
    }

    impl FakePmService {
        fn with_graph(graph: DependencyGraph) -> Self {
            Self {
                analyzer_available: true,
                result: Mutex::new(Some(Ok(graph))),
                requested_ids: Mutex::new(Vec::new()),
            }
        }

        fn unavailable() -> Self {
            Self {
                analyzer_available: false,
                result: Mutex::new(None),
                requested_ids: Mutex::new(Vec::new()),
            }
        }

        fn failing(message: &str) -> Self {
            Self {
                analyzer_available: true,
                result: Mutex::new(Some(Err(message.to_string()))),
                requested_ids: Mutex::new(Vec::new()),
            }
        }

        fn requested_ids(&self) -> Vec<String> {
            self.requested_ids.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl IssueGraphPm for FakePmService {
        fn analyzer_available(&self) -> bool {
            self.analyzer_available
        }

        async fn issue_subgraph_json(&self, id: &str) -> anyhow::Result<DependencyGraph> {
            self.requested_ids.lock().unwrap().push(id.to_string());
            match self.result.lock().unwrap().take().expect("fake result") {
                Ok(graph) => Ok(graph),
                Err(message) => Err(anyhow::anyhow!(message)),
            }
        }
    }

    fn dependency_graph() -> DependencyGraph {
        DependencyGraph {
            format: Some("json".into()),
            graph: None,
            nodes: 2,
            edges: 1,
            data_hash: Some("hash".into()),
            adjacency: Some(AdjacencyData {
                nodes: vec![
                    GraphNode {
                        id: "bd-root".into(),
                        title: Some("Root issue".into()),
                        status: Some("open".into()),
                        priority: Some(1),
                        labels: vec!["feature".into()],
                        pagerank: Some(0.5),
                    },
                    GraphNode {
                        id: "bd-child".into(),
                        title: Some("Child issue".into()),
                        status: Some("blocked".into()),
                        priority: Some(2),
                        labels: vec!["backend".into()],
                        pagerank: None,
                    },
                ],
                edges: Some(vec![GraphEdge {
                    from: "bd-root".into(),
                    to: "bd-child".into(),
                    edge_type: Some("depends_on".into()),
                }]),
            }),
            raw: serde_json::Value::Null,
        }
    }

    async fn next_event(events: &mut UnboundedReceiver<SpurEventBody>) -> SpurEventBody {
        tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("event timeout")
            .expect("event channel closed")
    }

    #[tokio::test]
    async fn get_issue_graph_emits_issue_subgraph_loaded() {
        let fake = FakePmService::with_graph(dependency_graph());
        let (funnel, mut events) = crate::event_funnel::test_channel();

        handle_get_issue_graph(Some(&fake), &funnel, "bd-root".into()).await;

        assert_eq!(fake.requested_ids(), vec!["bd-root"]);
        match next_event(&mut events).await {
            SpurEventBody::IssueSubgraphLoaded {
                requested_id,
                nodes,
                edges,
            } => {
                assert_eq!(requested_id, "bd-root");
                assert_eq!(nodes.len(), 2);
                assert_eq!(nodes[0].id, "bd-root");
                assert_eq!(nodes[0].title.as_deref(), Some("Root issue"));
                assert_eq!(nodes[0].labels, vec!["feature"]);
                assert_eq!(edges.len(), 1);
                assert_eq!(edges[0].from, "bd-root");
                assert_eq!(edges[0].to, "bd-child");
                assert_eq!(edges[0].edge_type.as_deref(), Some("depends_on"));
            }
            other => panic!("expected IssueSubgraphLoaded, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_issue_graph_emits_command_error_when_bv_unavailable() {
        let fake = FakePmService::unavailable();
        let (funnel, mut events) = crate::event_funnel::test_channel();

        handle_get_issue_graph(Some(&fake), &funnel, "bd-root".into()).await;

        assert!(fake.requested_ids().is_empty());
        match next_event(&mut events).await {
            SpurEventBody::IssueCommandError {
                operation,
                error,
                id,
            } => {
                assert_eq!(operation, "GetIssueGraph");
                assert_eq!(
                    error,
                    "Issue graph unavailable for configured issue tracker"
                );
                assert_eq!(id, Some("bd-root".into()));
            }
            other => panic!("expected IssueCommandError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_issue_graph_emits_command_error_when_subgraph_fails() {
        let fake = FakePmService::failing("bv failed");
        let (funnel, mut events) = crate::event_funnel::test_channel();

        handle_get_issue_graph(Some(&fake), &funnel, "bd-root".into()).await;

        assert_eq!(fake.requested_ids(), vec!["bd-root"]);
        match next_event(&mut events).await {
            SpurEventBody::IssueCommandError {
                operation,
                error,
                id,
            } => {
                assert_eq!(operation, "GetIssueGraph");
                assert_eq!(error, "bv failed");
                assert_eq!(id, Some("bd-root".into()));
            }
            other => panic!("expected IssueCommandError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_issue_graph_emits_command_error_when_pm_missing() {
        let (funnel, mut events) = crate::event_funnel::test_channel();

        handle_get_issue_graph(None::<&FakePmService>, &funnel, "bd-root".into()).await;

        match next_event(&mut events).await {
            SpurEventBody::IssueCommandError {
                operation,
                error,
                id,
            } => {
                assert_eq!(operation, "GetIssueGraph");
                assert_eq!(error, "No issue tracker configured");
                assert_eq!(id, Some("bd-root".into()));
            }
            other => panic!("expected IssueCommandError, got {other:?}"),
        }
    }
}
