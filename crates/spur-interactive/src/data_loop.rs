use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::{mpsc, Semaphore};
use tracing::Instrument;

use crate::host::DataQuery;
use spur_acp::SpurEventBody;

const GRAPH_CONCURRENCY_LIMIT: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum QueryKind {
    GetIssueDetail,
    GetIssueGraph,
}

impl QueryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::GetIssueDetail => "GetIssueDetail",
            Self::GetIssueGraph => "GetIssueGraph",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct QueryKey {
    kind: QueryKind,
    id: String,
}

impl QueryKey {
    fn from_query(query: &DataQuery) -> Self {
        match query {
            DataQuery::GetIssueDetail { id } => Self {
                kind: QueryKind::GetIssueDetail,
                id: id.clone(),
            },
            DataQuery::GetIssueGraph { id } => Self {
                kind: QueryKind::GetIssueGraph,
                id: id.clone(),
            },
        }
    }
}

pub(crate) trait DataQueryProvider: Send + Sync + 'static {
    fn get_issue<'a>(
        &'a self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<spur_pm::Issue>> + Send + 'a>>;

    fn issue_graph_available(&self) -> bool;

    fn issue_subgraph_json<'a>(
        &'a self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<spur_pm::graph::DependencyGraph>> + Send + 'a>>;
}

impl DataQueryProvider for spur_pm::PmService {
    fn get_issue<'a>(
        &'a self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<spur_pm::Issue>> + Send + 'a>> {
        Box::pin(async move { spur_pm::PmService::get_issue(self, id).await })
    }

    fn issue_graph_available(&self) -> bool {
        self.issue_graph_available()
    }

    fn issue_subgraph_json<'a>(
        &'a self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<spur_pm::graph::DependencyGraph>> + Send + 'a>>
    {
        Box::pin(async move { spur_pm::PmService::issue_subgraph_json(self, id).await })
    }
}

pub async fn run_data_query_loop(
    data_rx: mpsc::Receiver<DataQuery>,
    pm_service: Arc<spur_pm::PmService>,
    funnel: spur_core::event_funnel::FunnelHandle,
) {
    run_data_query_loop_with_provider(data_rx, Some(pm_service), funnel).await;
}

pub(crate) async fn run_data_query_loop_with_provider<P: DataQueryProvider>(
    mut data_rx: mpsc::Receiver<DataQuery>,
    pm_service: Option<Arc<P>>,
    funnel: spur_core::event_funnel::FunnelHandle,
) {
    let graph_semaphore = Arc::new(Semaphore::new(GRAPH_CONCURRENCY_LIMIT));
    let (completed_tx, mut completed_rx) = mpsc::unbounded_channel::<QueryKey>();
    let mut pending = HashSet::new();
    let mut data_rx_open = true;

    loop {
        if !data_rx_open && pending.is_empty() {
            break;
        }

        tokio::select! {
            maybe_query = data_rx.recv(), if data_rx_open => {
                match maybe_query {
                    Some(query) => {
                        let key = QueryKey::from_query(&query);
                        let kind = key.kind.as_str();
                        if !pending.insert(key.clone()) {
                            tracing::info!(
                                target: "issue_probe",
                                site = "data_loop_query_coalesced",
                                kind = kind,
                                id = %key.id,
                                "DataQuery dropped because an identical query is already pending",
                            );
                            continue;
                        }

                        spawn_data_query_handler(
                            query,
                            key,
                            pm_service.clone(),
                            funnel.clone(),
                            graph_semaphore.clone(),
                            completed_tx.clone(),
                        );
                    }
                    None => {
                        data_rx_open = false;
                    }
                }
            }
            maybe_completed = completed_rx.recv(), if !pending.is_empty() => {
                if let Some(key) = maybe_completed {
                    pending.remove(&key);
                }
            }
        }
    }
}

fn spawn_data_query_handler<P: DataQueryProvider>(
    query: DataQuery,
    key: QueryKey,
    pm_service: Option<Arc<P>>,
    funnel: spur_core::event_funnel::FunnelHandle,
    graph_semaphore: Arc<Semaphore>,
    completed_tx: mpsc::UnboundedSender<QueryKey>,
) {
    let kind = key.kind.as_str();
    let id = key.id.clone();
    let span = tracing::Span::current();

    tokio::spawn(
        async move {
            let started = Instant::now();
            let ok = match query {
                DataQuery::GetIssueDetail { id } => {
                    handle_get_issue_detail(pm_service.as_deref(), &funnel, id).await
                }
                DataQuery::GetIssueGraph { id } => {
                    let permit_started = Instant::now();
                    match graph_semaphore.acquire_owned().await {
                        Ok(_permit) => {
                            let semaphore_wait_ms = permit_started.elapsed().as_millis() as u64;
                            let ok =
                                handle_get_issue_graph(pm_service.as_deref(), &funnel, id).await;
                            tracing::debug!(
                                target: "issue_probe",
                                site = "data_loop_graph_permit_released",
                                kind = kind,
                                id = %key.id,
                                semaphore_wait_ms = semaphore_wait_ms,
                                "GetIssueGraph semaphore permit released",
                            );
                            ok
                        }
                        Err(error) => {
                            funnel.emit(SpurEventBody::IssueCommandError {
                                operation: "GetIssueGraph".into(),
                                error: format!("graph query semaphore closed: {error}"),
                                id: Some(id),
                            });
                            false
                        }
                    }
                }
            };

            tracing::info!(
                target: "issue_probe",
                site = "data_loop_query_done",
                kind = kind,
                id = %id,
                elapsed_ms = started.elapsed().as_millis() as u64,
                ok = ok,
                "DataQuery handler completed",
            );
            let _ = completed_tx.send(key);
        }
        .instrument(span),
    );
}

async fn handle_get_issue_detail<P: DataQueryProvider + ?Sized>(
    pm: Option<&P>,
    funnel: &spur_core::event_funnel::FunnelHandle,
    id: String,
) -> bool {
    let Some(pm) = pm else {
        funnel.emit(SpurEventBody::IssueCommandError {
            operation: "GetIssueDetail".into(),
            error: "No issue tracker configured".into(),
            id: Some(id),
        });
        return false;
    };

    match pm.get_issue(&id).await {
        Ok(issue) => {
            funnel.emit(SpurEventBody::IssueDetailFetched {
                requested_id: id,
                issue: issue_to_detail_event(&issue),
            });
            true
        }
        Err(error) => {
            funnel.emit(SpurEventBody::IssueCommandError {
                operation: "GetIssueDetail".into(),
                error: error.to_string(),
                id: Some(id),
            });
            false
        }
    }
}

// TODO: Extract this alongside the orchestrator copy once there is a shared
// event-conversion module that can be used by both spur-core and spur-interactive.
fn issue_to_detail_event(issue: &spur_pm::Issue) -> spur_acp::IssueDetailEvent {
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
        created_at: issue.created_at,
        updated_at: issue.updated_at,
    }
}

fn graph_node_to_event(node: &spur_pm::graph::GraphNode) -> spur_acp::GraphNodeEvent {
    spur_acp::GraphNodeEvent {
        id: node.id.clone(),
        title: node.title.clone(),
        status: node.status.clone(),
        priority: node.priority,
        labels: node.labels.clone(),
        pagerank: node.pagerank,
    }
}

fn graph_edge_to_event(edge: &spur_pm::graph::GraphEdge) -> spur_acp::GraphEdgeEvent {
    spur_acp::GraphEdgeEvent {
        from: edge.from.clone(),
        to: edge.to.clone(),
        edge_type: edge.edge_type.clone(),
    }
}

fn dependency_graph_to_event_parts(
    graph: spur_pm::graph::DependencyGraph,
) -> (Vec<spur_acp::GraphNodeEvent>, Vec<spur_acp::GraphEdgeEvent>) {
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

async fn handle_get_issue_graph<P: DataQueryProvider + ?Sized>(
    pm: Option<&P>,
    funnel: &spur_core::event_funnel::FunnelHandle,
    id: String,
) -> bool {
    let Some(pm) = pm else {
        funnel.emit(SpurEventBody::IssueCommandError {
            operation: "GetIssueGraph".into(),
            error: "No issue tracker configured".into(),
            id: Some(id),
        });
        return false;
    };

    if !pm.issue_graph_available() {
        funnel.emit(SpurEventBody::IssueCommandError {
            operation: "GetIssueGraph".into(),
            error: "Issue graph unavailable for configured issue tracker".into(),
            id: Some(id),
        });
        return false;
    }

    match pm.issue_subgraph_json(&id).await {
        Ok(graph) => {
            let (nodes, edges) = dependency_graph_to_event_parts(graph);
            funnel.emit(SpurEventBody::IssueSubgraphLoaded {
                requested_id: id,
                nodes,
                edges,
            });
            true
        }
        Err(error) => {
            funnel.emit(SpurEventBody::IssueCommandError {
                operation: "GetIssueGraph".into(),
                error: error.to_string(),
                id: Some(id),
            });
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use spur_acp::SpurEventBody;
    use spur_pm::graph::{AdjacencyData, DependencyGraph, GraphNode};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;
    use tokio::sync::{mpsc::UnboundedReceiver, mpsc::UnboundedSender, watch};

    struct FakeDataPm {
        detail_started_tx: Option<UnboundedSender<String>>,
        detail_release_rx: Option<watch::Receiver<bool>>,
        detail_calls: Mutex<Vec<String>>,
        graph_calls: Mutex<Vec<String>>,
        graph_delay: Duration,
        graph_in_flight: AtomicUsize,
        graph_max_in_flight: AtomicUsize,
        analyzer_available: bool,
    }

    impl FakeDataPm {
        fn new() -> Self {
            Self {
                detail_started_tx: None,
                detail_release_rx: None,
                detail_calls: Mutex::new(Vec::new()),
                graph_calls: Mutex::new(Vec::new()),
                graph_delay: Duration::ZERO,
                graph_in_flight: AtomicUsize::new(0),
                graph_max_in_flight: AtomicUsize::new(0),
                analyzer_available: true,
            }
        }

        fn with_detail_start_notifications(mut self, tx: UnboundedSender<String>) -> Self {
            self.detail_started_tx = Some(tx);
            self
        }

        fn with_detail_release(mut self, rx: watch::Receiver<bool>) -> Self {
            self.detail_release_rx = Some(rx);
            self
        }

        fn with_graph_delay(mut self, delay: Duration) -> Self {
            self.graph_delay = delay;
            self
        }

        fn detail_calls(&self) -> Vec<String> {
            self.detail_calls.lock().unwrap().clone()
        }

        fn graph_calls(&self) -> Vec<String> {
            self.graph_calls.lock().unwrap().clone()
        }

        fn max_graph_in_flight(&self) -> usize {
            self.graph_max_in_flight.load(Ordering::SeqCst)
        }
    }

    impl DataQueryProvider for FakeDataPm {
        fn get_issue<'a>(
            &'a self,
            id: &'a str,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<spur_pm::Issue>> + Send + 'a>> {
            Box::pin(async move {
                self.detail_calls.lock().unwrap().push(id.to_string());
                if let Some(tx) = &self.detail_started_tx {
                    let _ = tx.send(id.to_string());
                }
                if let Some(release_rx) = &self.detail_release_rx {
                    let mut release_rx = release_rx.clone();
                    while !*release_rx.borrow() {
                        release_rx.changed().await?;
                    }
                }
                Ok(issue(id))
            })
        }

        fn issue_graph_available(&self) -> bool {
            self.analyzer_available
        }

        fn issue_subgraph_json<'a>(
            &'a self,
            id: &'a str,
        ) -> Pin<
            Box<dyn Future<Output = anyhow::Result<spur_pm::graph::DependencyGraph>> + Send + 'a>,
        > {
            Box::pin(async move {
                self.graph_calls.lock().unwrap().push(id.to_string());
                let in_flight = self.graph_in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                self.graph_max_in_flight
                    .fetch_max(in_flight, Ordering::SeqCst);
                tokio::time::sleep(self.graph_delay).await;
                self.graph_in_flight.fetch_sub(1, Ordering::SeqCst);
                Ok(graph(id))
            })
        }
    }

    fn issue(id: &str) -> spur_pm::Issue {
        let now = Utc::now();
        spur_pm::Issue {
            id: id.to_string(),
            source: spur_pm::PmSource::Beads,
            title: format!("Issue {id}"),
            body: format!("Body {id}"),
            status: "open".into(),
            labels: Vec::new(),
            assignee: None,
            url: format!("https://example.test/{id}"),
            priority: None,
            issue_type: None,
            source_system: None,
            source_repo: None,
            external_ref: None,
            blocked_by: Vec::new(),
            due_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn graph(id: &str) -> DependencyGraph {
        DependencyGraph {
            format: Some("json".into()),
            graph: None,
            nodes: 1,
            edges: 0,
            data_hash: None,
            adjacency: Some(AdjacencyData {
                nodes: vec![GraphNode {
                    id: id.to_string(),
                    title: Some(format!("Issue {id}")),
                    status: Some("open".into()),
                    priority: None,
                    labels: Vec::new(),
                    pagerank: None,
                }],
                edges: Some(Vec::new()),
            }),
            raw: Default::default(),
        }
    }

    async fn next_event(events: &mut UnboundedReceiver<SpurEventBody>) -> SpurEventBody {
        tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("event timeout")
            .expect("event channel closed")
    }

    #[tokio::test]
    async fn data_loop_dispatches_independently() {
        let (query_tx, query_rx) = mpsc::channel(64);
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (release_tx, release_rx) = watch::channel(false);
        let pm = Arc::new(
            FakeDataPm::new()
                .with_detail_start_notifications(started_tx)
                .with_detail_release(release_rx),
        );
        let (funnel, mut events) = spur_core::event_funnel::test_channel();

        let loop_handle = tokio::spawn(run_data_query_loop_with_provider(
            query_rx,
            Some(pm.clone()),
            funnel,
        ));

        for n in 0..5 {
            query_tx
                .send(DataQuery::GetIssueDetail {
                    id: format!("bd-{n}"),
                })
                .await
                .unwrap();
        }

        let mut started = Vec::new();
        for _ in 0..5 {
            started.push(
                tokio::time::timeout(Duration::from_secs(1), started_rx.recv())
                    .await
                    .expect("detail start timeout")
                    .expect("detail start channel closed"),
            );
        }
        started.sort();
        assert_eq!(started, ["bd-0", "bd-1", "bd-2", "bd-3", "bd-4"]);

        release_tx.send(true).unwrap();

        let mut fetched = Vec::new();
        for _ in 0..5 {
            match next_event(&mut events).await {
                SpurEventBody::IssueDetailFetched { requested_id, .. } => {
                    fetched.push(requested_id)
                }
                other => panic!("expected IssueDetailFetched, got {other:?}"),
            }
        }
        fetched.sort();
        assert_eq!(fetched, ["bd-0", "bd-1", "bd-2", "bd-3", "bd-4"]);
        assert_eq!(pm.detail_calls().len(), 5);

        drop(query_tx);
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn data_loop_coalesces_same_id() {
        let (query_tx, query_rx) = mpsc::channel(64);
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let (release_tx, release_rx) = watch::channel(false);
        let pm = Arc::new(
            FakeDataPm::new()
                .with_detail_start_notifications(started_tx)
                .with_detail_release(release_rx),
        );
        let (funnel, mut events) = spur_core::event_funnel::test_channel();

        let loop_handle = tokio::spawn(run_data_query_loop_with_provider(
            query_rx,
            Some(pm.clone()),
            funnel,
        ));

        query_tx
            .send(DataQuery::GetIssueDetail { id: "bd-x".into() })
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), started_rx.recv())
                .await
                .expect("detail start timeout")
                .expect("detail start channel closed"),
            "bd-x"
        );

        query_tx
            .send(DataQuery::GetIssueDetail { id: "bd-x".into() })
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), started_rx.recv())
                .await
                .is_err(),
            "duplicate query should be dropped while the first is pending"
        );
        assert_eq!(pm.detail_calls(), vec!["bd-x"]);

        release_tx.send(true).unwrap();
        match next_event(&mut events).await {
            SpurEventBody::IssueDetailFetched { requested_id, .. } => {
                assert_eq!(requested_id, "bd-x");
            }
            other => panic!("expected IssueDetailFetched, got {other:?}"),
        }

        drop(query_tx);
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn data_loop_graph_concurrency_bounded() {
        let (query_tx, query_rx) = mpsc::channel(64);
        let pm = Arc::new(FakeDataPm::new().with_graph_delay(Duration::from_millis(100)));
        let (funnel, mut events) = spur_core::event_funnel::test_channel();

        let loop_handle = tokio::spawn(run_data_query_loop_with_provider(
            query_rx,
            Some(pm.clone()),
            funnel,
        ));

        for n in 0..8 {
            query_tx
                .send(DataQuery::GetIssueGraph {
                    id: format!("bd-g{n}"),
                })
                .await
                .unwrap();
        }

        let mut loaded = Vec::new();
        for _ in 0..8 {
            match next_event(&mut events).await {
                SpurEventBody::IssueSubgraphLoaded { requested_id, .. } => {
                    loaded.push(requested_id)
                }
                other => panic!("expected IssueSubgraphLoaded, got {other:?}"),
            }
        }
        loaded.sort();
        assert_eq!(
            loaded,
            ["bd-g0", "bd-g1", "bd-g2", "bd-g3", "bd-g4", "bd-g5", "bd-g6", "bd-g7",]
        );
        assert_eq!(pm.graph_calls().len(), 8);
        assert!(
            pm.max_graph_in_flight() <= 4,
            "expected at most 4 concurrent graph queries, saw {}",
            pm.max_graph_in_flight()
        );

        drop(query_tx);
        loop_handle.await.unwrap();
    }
}
