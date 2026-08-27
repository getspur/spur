use std::collections::BTreeSet;

use spur_graph::memory_eval::ranking::{
    oracle_ranking, Bm25Ranker, ChronologyKey, CorpusDocument, Granularity, OracleRequest,
    QueryOccurrenceId, RankRequest, Ranker, RankingSet, RecentRanker, Variant,
};

fn query_occurrence_id(source_id: &str) -> QueryOccurrenceId {
    QueryOccurrenceId::new(source_id)
}

fn fixture_corpus() -> Vec<CorpusDocument> {
    vec![
        CorpusDocument {
            occurrence_id: "occ_c".to_owned(),
            text: "shared token".to_owned(),
            chronology_key: Some(ChronologyKey::new(20260825)),
        },
        CorpusDocument {
            occurrence_id: "occ_a".to_owned(),
            text: "shared token".to_owned(),
            chronology_key: Some(ChronologyKey::new(20260827)),
        },
        CorpusDocument {
            occurrence_id: "occ_b".to_owned(),
            text: "shared token".to_owned(),
            chronology_key: Some(ChronologyKey::new(20260826)),
        },
        CorpusDocument {
            occurrence_id: "occ_c".to_owned(),
            text: "shared token".to_owned(),
            chronology_key: Some(ChronologyKey::new(20260825)),
        },
    ]
}

fn request<'a>(corpus: &'a [CorpusDocument], granularity: Granularity) -> RankRequest<'a> {
    RankRequest {
        query: "shared token",
        granularity,
        corpus,
    }
}

fn occurrence_ids(ranking: &spur_graph::memory_eval::ranking::Ranking) -> Vec<&str> {
    ranking
        .hits
        .iter()
        .map(|hit| hit.occurrence_id.as_str())
        .collect()
}

#[test]
fn non_oracle_request_serialization_contains_no_gold_fields() {
    let corpus = fixture_corpus();
    let json = serde_json::to_string(&request(&corpus, Granularity::Turn)).unwrap();

    for forbidden in [
        "answer",
        "evidence",
        "has_answer",
        "question_type",
        "answer_session",
        "gold_session",
        "gold_turn",
    ] {
        assert!(!json.contains(forbidden), "leaked {forbidden}: {json}");
    }
}

#[test]
fn ranker_boundary_is_identifier_free_until_the_caller_keys_the_artifact() {
    const Q_001_ABS_SHA256: &str =
        "ffc684fdb2f7486421672e52aa7bce75fe3301d8ae14d0efc74d551474d8c2d9";

    let corpus = fixture_corpus();
    let rank_request = RankRequest {
        query: "shared token",
        granularity: Granularity::Turn,
        corpus: &corpus,
    };

    let json = serde_json::to_string(&rank_request).unwrap();
    let serialized_request = serde_json::from_str::<serde_json::Value>(&json).unwrap();
    let fields = serialized_request
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();

    assert_eq!(fields, BTreeSet::from(["corpus", "granularity", "query"]));
    assert!(
        !json.contains("query_occurrence_id"),
        "leaked ID field: {json}"
    );
    assert!(!json.contains(Q_001_ABS_SHA256), "leaked ID digest: {json}");

    let ranking = RecentRanker.rank(&rank_request, 2).unwrap();
    let ranking_json = serde_json::to_string(&ranking).unwrap();
    assert!(
        !ranking_json.contains("query_occurrence_id"),
        "ranker keyed its output: {ranking_json}"
    );
    assert!(
        !ranking_json.contains(Q_001_ABS_SHA256),
        "ranker copied the ID digest: {ranking_json}"
    );

    let gold = vec!["occ_a".to_owned()];
    let oracle = oracle_ranking(
        &OracleRequest {
            request: rank_request,
            gold_occurrence_ids: &gold,
        },
        1,
    );
    let oracle_json = serde_json::to_string(&oracle).unwrap();
    assert!(
        !oracle_json.contains("query_occurrence_id"),
        "oracle keyed its output: {oracle_json}"
    );
    assert!(
        !oracle_json.contains(Q_001_ABS_SHA256),
        "oracle copied the ID digest: {oracle_json}"
    );

    let query_occurrence_id = query_occurrence_id("q-001_abs");
    let mut rankings = RankingSet::new();
    assert!(rankings.is_empty());
    rankings.insert(
        (
            query_occurrence_id.clone(),
            Variant::Recent,
            Granularity::Turn,
        ),
        ranking,
    );
    assert!(rankings.contains_key(&(query_occurrence_id, Variant::Recent, Granularity::Turn,)));
}

#[test]
fn bm25_ties_are_stable_and_top_k_ids_are_unique() {
    let corpus = fixture_corpus();
    let ranker = Bm25Ranker::build(corpus.clone()).unwrap();
    let rank_request = request(&corpus, Granularity::Turn);

    let first = ranker.rank(&rank_request, 3).unwrap();
    let second = ranker.rank(&rank_request, 3).unwrap();

    assert_eq!(first, second);
    assert_eq!(occurrence_ids(&first), ["occ_a", "occ_b", "occ_c"]);
    assert_eq!(
        first
            .hits
            .iter()
            .map(|hit| &hit.occurrence_id)
            .collect::<BTreeSet<_>>()
            .len(),
        first.hits.len()
    );
    assert_eq!(first.k, 3);
}

#[test]
fn bm25_prefers_matching_documents() {
    let mut corpus = fixture_corpus();
    corpus[2].text = "orchid orchid orchid".to_owned();
    let ranker = Bm25Ranker::build(corpus.clone()).unwrap();
    let rank_request = RankRequest {
        query: "orchid",
        granularity: Granularity::Turn,
        corpus: &corpus,
    };

    let ranking = ranker.rank(&rank_request, 1).unwrap();

    assert_eq!(occurrence_ids(&ranking), ["occ_b"]);
}

#[test]
fn recent_is_newest_first_with_occurrence_id_ties() {
    let corpus = vec![
        CorpusDocument {
            occurrence_id: "tie_z".to_owned(),
            text: "z".to_owned(),
            chronology_key: Some(ChronologyKey::new(20260827)),
        },
        CorpusDocument {
            occurrence_id: "old".to_owned(),
            text: "old".to_owned(),
            chronology_key: Some(ChronologyKey::new(20260826)),
        },
        CorpusDocument {
            occurrence_id: "tie_a".to_owned(),
            text: "a".to_owned(),
            chronology_key: Some(ChronologyKey::new(20260827)),
        },
        CorpusDocument {
            occurrence_id: "undated".to_owned(),
            text: "none".to_owned(),
            chronology_key: None,
        },
    ];

    let first = RecentRanker
        .rank(&request(&corpus, Granularity::Session), 4)
        .unwrap();
    let second = RecentRanker
        .rank(&request(&corpus, Granularity::Session), 4)
        .unwrap();

    assert_eq!(first, second);
    assert_eq!(occurrence_ids(&first), ["tie_a", "tie_z", "old", "undated"]);
}

#[test]
fn recent_uses_real_locomo_chronology_instead_of_raw_string_order() {
    let corpus = vec![
        CorpusDocument {
            occurrence_id: "locomo-july".to_owned(),
            text: "older LoCoMo turn".to_owned(),
            // "8:18 pm on 6 July, 2023"
            chronology_key: Some(ChronologyKey::new(202307062018)),
        },
        CorpusDocument {
            occurrence_id: "locomo-december".to_owned(),
            text: "newer LoCoMo turn".to_owned(),
            // "10:04 am on 19 December, 2023"
            chronology_key: Some(ChronologyKey::new(202312191004)),
        },
    ];

    let ranking = RecentRanker
        .rank(&request(&corpus, Granularity::Turn), 2)
        .unwrap();

    assert_eq!(occurrence_ids(&ranking), ["locomo-december", "locomo-july"]);
}

#[test]
fn oracle_gold_uses_a_separate_request_and_returns_only_corpus_occurrences() {
    let corpus = fixture_corpus();
    let gold = vec![
        "occ_c".to_owned(),
        "occ_a".to_owned(),
        "occ_c".to_owned(),
        "not-in-corpus".to_owned(),
    ];
    let oracle_request = OracleRequest {
        request: request(&corpus, Granularity::Turn),
        gold_occurrence_ids: &gold,
    };

    let ranking = oracle_ranking(&oracle_request, 3);

    assert_eq!(ranking.variant, Variant::Oracle);
    assert_eq!(occurrence_ids(&ranking), ["occ_c", "occ_a"]);
}

#[test]
fn turn_and_session_rankings_are_distinct_artifacts_with_declared_k() {
    let corpus = fixture_corpus();
    let turn = RecentRanker
        .rank(&request(&corpus, Granularity::Turn), 2)
        .unwrap();
    let session = RecentRanker
        .rank(&request(&corpus, Granularity::Session), 2)
        .unwrap();
    let mut rankings = RankingSet::new();
    rankings.insert(
        (
            query_occurrence_id("q-1"),
            Variant::Recent,
            Granularity::Turn,
        ),
        turn,
    );
    rankings.insert(
        (
            query_occurrence_id("q-1"),
            Variant::Recent,
            Granularity::Session,
        ),
        session,
    );

    assert_eq!(rankings.len(), 2);
    assert!(rankings.values().all(|ranking| ranking.k == 2));
}

#[test]
fn hashes_are_stable_shared_between_non_oracles_and_bind_query_and_corpus() {
    let corpus = fixture_corpus();
    let rank_request = request(&corpus, Granularity::Turn);
    let recent = RecentRanker.rank(&rank_request, 2).unwrap();
    let bm25 = Bm25Ranker::build(corpus.clone())
        .unwrap()
        .rank(&rank_request, 2)
        .unwrap();

    assert_eq!(recent.query_sha256, bm25.query_sha256);
    assert_eq!(recent.corpus_sha256, bm25.corpus_sha256);
    assert_eq!(recent.serialization_sha256, bm25.serialization_sha256);
    for hash in [
        &recent.query_sha256,
        &recent.corpus_sha256,
        &recent.serialization_sha256,
    ] {
        assert_eq!(hash.len(), 64);
        assert!(hash.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    let changed_query = RankRequest {
        query: "different tokens",
        granularity: Granularity::Turn,
        corpus: &corpus,
    };
    let changed_query_ranking = RecentRanker.rank(&changed_query, 2).unwrap();
    assert_ne!(recent.query_sha256, changed_query_ranking.query_sha256);
    assert_eq!(recent.corpus_sha256, changed_query_ranking.corpus_sha256);
    assert_ne!(
        recent.serialization_sha256,
        changed_query_ranking.serialization_sha256
    );

    let mut changed_corpus = corpus.clone();
    changed_corpus[0].text.push_str(" changed");
    let changed_corpus_ranking = RecentRanker
        .rank(&request(&changed_corpus, Granularity::Turn), 2)
        .unwrap();
    assert_ne!(recent.corpus_sha256, changed_corpus_ranking.corpus_sha256);
    assert_ne!(
        recent.serialization_sha256,
        changed_corpus_ranking.serialization_sha256
    );

    let mut changed_chronology = corpus.clone();
    changed_chronology[0].chronology_key = Some(ChronologyKey::new(20260828));
    let changed_chronology_ranking = RecentRanker
        .rank(&request(&changed_chronology, Granularity::Turn), 2)
        .unwrap();
    assert_ne!(
        recent.corpus_sha256,
        changed_chronology_ranking.corpus_sha256
    );
}
