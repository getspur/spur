use serde_json::Value;
use spur_graph::EMBEDDING_VECTOR_DIMENSIONS;

const BM25_HIGH_CONFIDENCE_SCORE: f64 = 8.0;
const BM25_MEDIUM_CONFIDENCE_SCORE: f64 = 3.0;
const HYBRID_HIGH_CONFIDENCE_SCORE: f64 = 0.80;
const HYBRID_MEDIUM_CONFIDENCE_SCORE: f64 = 0.55;

pub(crate) fn format_query_vec_sql(query_vec: Option<&[f32]>) -> Option<String> {
    let query_vec = query_vec?;
    if query_vec.len() != EMBEDDING_VECTOR_DIMENSIONS
        || query_vec.iter().any(|value| !value.is_finite())
    {
        return None;
    }

    let mut sql = String::from("[");
    for (index, value) in query_vec.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push_str(&value.to_string());
    }
    sql.push_str("]::FLOAT[");
    sql.push_str(&EMBEDDING_VECTOR_DIMENSIONS.to_string());
    sql.push(']');
    Some(sql)
}

pub(crate) fn confidence_score_thresholds(grounding: Option<&str>) -> (f64, f64) {
    match grounding {
        Some(grounding) if grounding.starts_with("hybrid-") => {
            (HYBRID_HIGH_CONFIDENCE_SCORE, HYBRID_MEDIUM_CONFIDENCE_SCORE)
        }
        _ => (BM25_HIGH_CONFIDENCE_SCORE, BM25_MEDIUM_CONFIDENCE_SCORE),
    }
}

pub(crate) fn evidence_confidence(
    primary_evidence: &[Value],
    supporting_docs: &[Value],
) -> &'static str {
    let top_evidence = primary_evidence.first();
    let top_score = top_evidence
        .and_then(|evidence| evidence.get("score").and_then(Value::as_f64))
        .unwrap_or(0.0);
    let top_grounding =
        top_evidence.and_then(|evidence| evidence.get("grounding").and_then(Value::as_str));
    let (high_score, medium_score) = confidence_score_thresholds(top_grounding);
    let evidence_count = primary_evidence.len() + supporting_docs.len();

    if top_score > high_score && evidence_count >= 3 {
        "high"
    } else if top_score > medium_score && evidence_count >= 2 {
        "medium"
    } else {
        "low"
    }
}
