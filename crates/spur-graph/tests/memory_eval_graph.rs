use std::collections::BTreeSet;

use serde_json::json;
use spur_graph::memory_eval::memory_graph::{
    GraphIndexOnlyRanker, GraphTraversalRanker, MemoryGraph, MemoryRelation, MemorySession,
    MemoryTurn, TraversalConfig,
};
use spur_graph::memory_eval::ranking::{
    Bm25Ranker, ChronologyKey, CorpusDocument, Granularity, RankRequest, Ranker, Variant,
};
use spur_graph::RelationKind;

#[derive(Clone)]
struct FixtureCorpus {
    sessions: Vec<MemorySession>,
    questions: Vec<serde_json::Value>,
}

impl FixtureCorpus {
    fn records(&self) -> &[MemorySession] {
        &self.sessions
    }

    fn with_questions(mut self) -> Self {
        self.questions = vec![json!({
            "question": "Which secret gold answer should be returned?",
            "answer": "never index this",
            "evidence": ["turn-a"]
        })];
        self
    }
}

fn document(id: &str, text: &str, chronology: i64) -> CorpusDocument {
    CorpusDocument {
        occurrence_id: id.to_owned(),
        text: text.to_owned(),
        chronology_key: Some(ChronologyKey::new(chronology)),
    }
}

fn fixture_corpus() -> FixtureCorpus {
    FixtureCorpus {
        sessions: vec![
            MemorySession {
                document: document("session-a", "bicycle planning", 10),
                turns: vec![
                    MemoryTurn {
                        document: document("turn-a", "the bicycle is blue", 10),
                        speaker: Some("Alice".to_owned()),
                    },
                    MemoryTurn {
                        document: document("turn-b", "the route crosses downtown", 11),
                        speaker: Some("Bob".to_owned()),
                    },
                ],
            },
            MemorySession {
                document: document("session-b", "garden planning", 20),
                turns: vec![MemoryTurn {
                    document: document("turn-c", "the garden has roses", 20),
                    speaker: Some("Alice".to_owned()),
                }],
            },
        ],
        questions: Vec::new(),
    }
}

fn turn_request<'a>(graph: &'a MemoryGraph, query: &'a str) -> RankRequest<'a> {
    RankRequest {
        query,
        granularity: Granularity::Turn,
        corpus: graph.corpus(Granularity::Turn),
    }
}

fn allowed_relations() -> BTreeSet<MemoryRelation> {
    BTreeSet::from([
        MemoryRelation::Contains,
        MemoryRelation::NextTurn,
        MemoryRelation::PreviousTurn,
        MemoryRelation::SpokenBy,
    ])
}

#[test]
fn graph_is_identical_before_and_after_questions_are_attached() {
    let corpus = fixture_corpus();
    let without_questions = MemoryGraph::build(corpus.records()).unwrap();
    let corpus = corpus.with_questions();
    let with_questions = MemoryGraph::build(corpus.records()).unwrap();

    assert_eq!(
        without_questions.content_hash(),
        with_questions.content_hash()
    );
    assert_eq!(without_questions.facts, with_questions.facts);
    assert!(without_questions
        .facts
        .nodes
        .iter()
        .all(|node| !node.label.contains("secret gold")));
}

#[test]
fn graph_facts_have_deterministic_session_turn_speaker_and_chronology_shape() {
    let graph = MemoryGraph::build(fixture_corpus().records()).unwrap();
    let stable_keys = graph
        .facts
        .nodes
        .iter()
        .map(|node| node.stable_key.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        stable_keys,
        vec![
            "memory:session:session-a",
            "memory:chronology:session-a",
            "memory:turn:turn-a",
            "memory:speaker:Alice",
            "memory:turn:turn-b",
            "memory:speaker:Bob",
            "memory:session:session-b",
            "memory:chronology:session-b",
            "memory:turn:turn-c",
        ]
    );
    assert_eq!(
        graph.relations().collect::<Vec<_>>(),
        vec![
            MemoryRelation::Contains,
            MemoryRelation::Contains,
            MemoryRelation::SpokenBy,
            MemoryRelation::Contains,
            MemoryRelation::NextTurn,
            MemoryRelation::PreviousTurn,
            MemoryRelation::SpokenBy,
            MemoryRelation::Contains,
            MemoryRelation::Contains,
            MemoryRelation::SpokenBy,
        ]
    );
    let direction_flags = graph
        .relations()
        .zip(&graph.facts.edges)
        .map(|(relation, edge)| (relation, edge.directed))
        .collect::<Vec<_>>();
    assert_eq!(
        direction_flags,
        vec![
            (MemoryRelation::Contains, false),
            (MemoryRelation::Contains, false),
            (MemoryRelation::SpokenBy, false),
            (MemoryRelation::Contains, false),
            (MemoryRelation::NextTurn, true),
            (MemoryRelation::PreviousTurn, true),
            (MemoryRelation::SpokenBy, false),
            (MemoryRelation::Contains, false),
            (MemoryRelation::Contains, false),
            (MemoryRelation::SpokenBy, false),
        ]
    );
    assert_eq!(
        serde_json::to_string(&direction_flags).unwrap(),
        r#"[["contains",false],["contains",false],["spoken_by",false],["contains",false],["next_turn",true],["previous_turn",true],["spoken_by",false],["contains",false],["contains",false],["spoken_by",false]]"#
    );
    assert_eq!(
        graph.content_hash(),
        MemoryGraph::build(fixture_corpus().records())
            .unwrap()
            .content_hash()
    );
}

#[test]
fn reviewer_runtime_traversal_has_no_reverse_arcs_hidden_from_graph_facts() {
    let graph = MemoryGraph::build(fixture_corpus().records()).unwrap();
    let alice_node = graph
        .facts
        .nodes
        .iter()
        .find(|node| node.stable_key == "memory:speaker:Alice")
        .unwrap()
        .node_id;
    let mut recorded_neighbors = BTreeSet::new();

    for edge in &graph.facts.edges {
        if edge.relation != RelationKind::Binds {
            continue;
        }
        let target = edge.target_node_id.unwrap();
        if edge.source_node_id == alice_node {
            recorded_neighbors.insert(target);
        }
        if !edge.directed && target == alice_node {
            recorded_neighbors.insert(edge.source_node_id);
        }
    }

    let ranker = GraphTraversalRanker::new(
        graph,
        TraversalConfig {
            seed_k: 1,
            max_depth: 1,
            relations: BTreeSet::from([MemoryRelation::SpokenBy]),
        },
    )
    .unwrap();
    let request = turn_request(ranker.graph(), "Alice");
    let (ranking, telemetry) = ranker.rank_with_telemetry(&request, 2).unwrap();

    assert_eq!(ranking.hits.len(), 2);
    for hit in &ranking.hits {
        let stable_key = hit.provenance_id.as_deref().unwrap();
        let node_id = ranker
            .graph()
            .facts
            .nodes
            .iter()
            .find(|node| node.stable_key == stable_key)
            .unwrap()
            .node_id;
        assert!(
            recorded_neighbors.contains(&node_id),
            "runtime returned {stable_key} through an arc absent from GraphFacts"
        );
    }
    assert_eq!(telemetry.traversed_edges, recorded_neighbors.len());
}

#[test]
fn traversal_returns_unique_canonical_provenance_not_internal_nodes() {
    let graph = MemoryGraph::build(fixture_corpus().records()).unwrap();
    let config = TraversalConfig {
        seed_k: 1,
        max_depth: 1,
        relations: BTreeSet::from([MemoryRelation::SpokenBy]),
    };
    let ranker = GraphTraversalRanker::new(graph, config).unwrap();
    let request = turn_request(ranker.graph(), "Alice");
    let (ranking, telemetry) = ranker.rank_with_telemetry(&request, 2).unwrap();

    assert_eq!(ranking.variant, Variant::GraphTraversal);
    assert_eq!(ranking.hits.len(), 2);
    assert!(ranking.hits.iter().all(|hit| hit.provenance_id.is_some()));
    assert_eq!(
        ranking
            .hits
            .iter()
            .map(|hit| &hit.occurrence_id)
            .collect::<BTreeSet<_>>()
            .len(),
        ranking.hits.len()
    );
    assert_eq!(
        ranking
            .hits
            .iter()
            .map(|hit| hit.occurrence_id.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["turn-a", "turn-c"])
    );
    assert!(telemetry.traversed_edges >= 2);
}

#[test]
fn graph_index_only_performs_zero_query_time_edge_traversal() {
    let graph = MemoryGraph::build(fixture_corpus().records()).unwrap();
    let ranker = GraphIndexOnlyRanker::new(graph).unwrap();
    let request = turn_request(ranker.graph(), "blue bicycle");
    let (ranking, telemetry) = ranker.rank_with_telemetry(&request, 2).unwrap();

    assert_eq!(ranking.variant, Variant::GraphIndexOnly);
    assert_eq!(ranking.hits[0].occurrence_id, "turn-a");
    assert_eq!(telemetry.traversed_edges, 0);
    assert_eq!(telemetry.visited_nodes, 0);
    assert_eq!(telemetry.scored_nodes, ranker.graph().facts.nodes.len());
}

#[test]
fn graph_index_only_respects_a_zero_provenance_budget() {
    let graph = MemoryGraph::build(fixture_corpus().records()).unwrap();
    let ranker = GraphIndexOnlyRanker::new(graph).unwrap();
    let request = turn_request(ranker.graph(), "blue bicycle");

    let ranking = ranker.rank(&request, 0).unwrap();

    assert!(ranking.hits.is_empty());
}

#[test]
fn traversal_config_has_an_exact_manifest_serialization_and_no_serde_defaults() {
    let config = TraversalConfig {
        seed_k: 2,
        max_depth: 2,
        relations: BTreeSet::from([
            MemoryRelation::Contains,
            MemoryRelation::NextTurn,
            MemoryRelation::SpokenBy,
        ]),
    };

    assert_eq!(
        serde_json::to_string(&config).unwrap(),
        r#"{"seed_k":2,"max_depth":2,"relations":["contains","next_turn","spoken_by"]}"#
    );
    assert!(serde_json::from_value::<TraversalConfig>(json!({
        "seed_k": 2,
        "relations": ["contains"]
    }))
    .is_err());
}

#[test]
fn graph_variants_preserve_the_shared_query_corpus_and_tokenization_hashes() {
    let graph = MemoryGraph::build(fixture_corpus().records()).unwrap();
    let flat = Bm25Ranker::build(graph.corpus(Granularity::Turn).to_vec()).unwrap();
    let request = turn_request(&graph, "blue bicycle");
    let flat_ranking = flat.rank(&request, 2).unwrap();
    let graph_ranking = GraphIndexOnlyRanker::new(graph.clone())
        .unwrap()
        .rank(&request, 2)
        .unwrap();
    let traversal_ranking = GraphTraversalRanker::new(
        graph.clone(),
        TraversalConfig {
            seed_k: 2,
            max_depth: 1,
            relations: allowed_relations(),
        },
    )
    .unwrap()
    .rank(&request, 2)
    .unwrap();

    for ranking in [graph_ranking, traversal_ranking] {
        assert_eq!(ranking.query_sha256, flat_ranking.query_sha256);
        assert_eq!(ranking.corpus_sha256, flat_ranking.corpus_sha256);
        assert_eq!(
            ranking.serialization_sha256,
            flat_ranking.serialization_sha256
        );
    }
}

#[test]
fn provenance_budget_property_holds_for_every_requested_k() {
    let graph = MemoryGraph::build(fixture_corpus().records()).unwrap();
    let ranker = GraphTraversalRanker::new(
        graph,
        TraversalConfig {
            seed_k: 9,
            max_depth: 2,
            relations: allowed_relations(),
        },
    )
    .unwrap();

    for k in 0..=8 {
        let request = turn_request(ranker.graph(), "Alice planning");
        let ranking = ranker.rank(&request, k).unwrap();
        let unique = ranking
            .hits
            .iter()
            .map(|hit| hit.occurrence_id.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(unique.len(), ranking.hits.len());
        assert!(ranking.hits.len() <= k);
        assert!(ranking.hits.iter().all(|hit| {
            hit.provenance_id
                .as_deref()
                .and_then(|node| ranker.graph().resolve_provenance(node))
                == Some((hit.occurrence_id.as_str(), Granularity::Turn))
        }));
    }
}

#[test]
fn graph_rankers_expose_build_query_and_index_size_telemetry() {
    let graph = MemoryGraph::build(fixture_corpus().records()).unwrap();
    let graph_build = graph.build_telemetry();
    let index_only = GraphIndexOnlyRanker::new(graph.clone()).unwrap();
    let traversal = GraphTraversalRanker::new(
        graph,
        TraversalConfig {
            seed_k: 2,
            max_depth: 2,
            relations: allowed_relations(),
        },
    )
    .unwrap();

    assert_eq!(
        index_only.build_telemetry().graph_build_nanoseconds,
        graph_build.graph_build_nanoseconds
    );
    assert!(index_only.build_telemetry().index_size_bytes > 0);
    assert!(traversal.build_telemetry().index_size_bytes > 0);

    let index_request = turn_request(index_only.graph(), "garden");
    let (_, index_query) = index_only.rank_with_telemetry(&index_request, 2).unwrap();
    let traversal_request = turn_request(traversal.graph(), "garden");
    let (_, traversal_query) = traversal
        .rank_with_telemetry(&traversal_request, 2)
        .unwrap();

    for telemetry in [index_query, traversal_query] {
        let serialized = serde_json::to_value(telemetry).unwrap();
        assert!(serialized["query_nanoseconds"].is_number());
    }
    assert!(traversal_query.visited_nodes > 0);
}

#[test]
fn graph_build_rejects_noncanonical_duplicate_occurrence_ids() {
    let mut corpus = fixture_corpus();
    corpus.sessions[1].turns[0].document.occurrence_id = "turn-a".to_owned();

    let error = MemoryGraph::build(corpus.records()).unwrap_err();

    assert!(error
        .to_string()
        .contains("duplicate occurrence ID `turn-a`"));
}
