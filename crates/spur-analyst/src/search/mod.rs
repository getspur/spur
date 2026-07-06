pub mod context_candidates;
pub mod graph_candidates;
pub mod hybrid;

#[cfg(test)]
mod tests {
    use spur_graph::EMBEDDING_VECTOR_DIMENSIONS;

    use crate::{KnowledgeCandidate, KnowledgeQueryResult};

    #[test]
    fn search_modules_expose_candidate_retrieval_boundaries() {
        assert!(super::hybrid::format_query_vec_sql(Some(&vec![
            0.0;
            EMBEDDING_VECTOR_DIMENSIONS - 1
        ]))
        .is_none());

        let mut result = KnowledgeQueryResult {
            db_path: "fixture.duckdb".to_owned(),
            graph_content_hash: Some("fixture-hash".to_owned()),
            candidates: vec![KnowledgeCandidate {
                kind: "code".to_owned(),
                title: "bm25".to_owned(),
                file_path: "src/lib.rs".to_owned(),
                stable_symbol_id: Some("sym-1".to_owned()),
                symbol_kind: Some("function".to_owned()),
                score: 0.5,
                signal: None,
                neighbor_kind: None,
                edge_bind_method: None,
                grounding: "bm25-code".to_owned(),
            }],
        };
        super::graph_candidates::merge_graph_candidates(
            &mut result,
            KnowledgeQueryResult {
                db_path: "fixture.duckdb".to_owned(),
                graph_content_hash: Some("fixture-hash".to_owned()),
                candidates: vec![KnowledgeCandidate {
                    kind: "code".to_owned(),
                    title: "graph".to_owned(),
                    file_path: "src/lib.rs".to_owned(),
                    stable_symbol_id: Some("sym-1".to_owned()),
                    symbol_kind: Some("function".to_owned()),
                    score: 0.9,
                    signal: None,
                    neighbor_kind: Some("primary".to_owned()),
                    edge_bind_method: None,
                    grounding: "graph".to_owned(),
                }],
            },
        );

        assert_eq!(result.candidates[0].title, "graph");
    }

    #[test]
    fn format_query_vec_sql_rejects_wrong_dimension() {
        assert!(super::hybrid::format_query_vec_sql(Some(&vec![
            0.0;
            EMBEDDING_VECTOR_DIMENSIONS - 1
        ]))
        .is_none());
        assert!(super::hybrid::format_query_vec_sql(Some(&vec![
            0.0;
            EMBEDDING_VECTOR_DIMENSIONS + 1
        ]))
        .is_none());
        assert!(
            super::hybrid::format_query_vec_sql(Some(&vec![0.0; EMBEDDING_VECTOR_DIMENSIONS]))
                .is_some()
        );
    }
}
