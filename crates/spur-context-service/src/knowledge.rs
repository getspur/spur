//! Knowledge-context retrieval for external packages.

use anyhow::{anyhow, Context as _, Result};
use duckdb::{params, Connection, Row};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

const EMBEDDING_VECTOR_DIMENSIONS: usize = 768;
const MAX_KNOWLEDGE_LIMIT: usize = 20;
const BM25_HIGH_CONFIDENCE_SCORE: f64 = 8.0;
const BM25_MEDIUM_CONFIDENCE_SCORE: f64 = 3.0;
const HYBRID_HIGH_CONFIDENCE_SCORE: f64 = 0.80;
const HYBRID_MEDIUM_CONFIDENCE_SCORE: f64 = 0.55;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeScope {
    Code,
    Docs,
    All,
}

impl KnowledgeScope {
    fn as_sql_scope(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Docs => "docs",
            Self::All => "all",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnowledgeContextOptions {
    pub query: String,
    pub source: String,
    pub package: String,
    pub revision: String,
    pub scope: KnowledgeScope,
    pub limit: usize,
    pub query_vec: Option<Vec<f32>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KnowledgeContextResult {
    pub primary_evidence: Vec<KnowledgeEvidence>,
    pub supporting_docs: Vec<KnowledgeEvidence>,
    pub confidence: String,
    pub answerable: bool,
    pub graph_content_hash: Option<String>,
    pub candidates: KnowledgeCandidateSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KnowledgeEvidence {
    pub kind: String,
    pub title: String,
    pub file: String,
    pub stable_symbol_id: Option<String>,
    pub symbol_kind: Option<String>,
    pub score: f64,
    pub signal: Option<String>,
    pub neighbor_kind: Option<String>,
    pub edge_bind_method: Option<String>,
    pub grounding: String,
    pub why_relevant: String,
    pub next: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KnowledgeCandidateSummary {
    pub total: usize,
    pub returned_primary: usize,
    pub returned_supporting_docs: usize,
    pub total_code: usize,
    pub total_docs: usize,
}

#[derive(Debug, Clone)]
struct KnowledgeCandidate {
    kind: String,
    title: String,
    file_path: String,
    stable_symbol_id: Option<String>,
    qualified_name: Option<String>,
    symbol_kind: Option<String>,
    score: f64,
    signal: Option<String>,
    neighbor_kind: Option<String>,
    edge_bind_method: Option<String>,
    grounding: String,
}

pub fn query_knowledge_context(
    db: &Connection,
    opts: &KnowledgeContextOptions,
) -> Result<KnowledgeContextResult> {
    let query = opts.query.trim();
    if query.is_empty() {
        return Err(anyhow!(
            "external knowledge context query must be non-empty"
        ));
    }
    if opts.source.trim().is_empty()
        || opts.package.trim().is_empty()
        || opts.revision.trim().is_empty()
    {
        return Err(anyhow!(
            "external knowledge context requires source, package, and revision"
        ));
    }

    let limit = opts.limit.clamp(1, MAX_KNOWLEDGE_LIMIT);
    let pool_limit = (limit * 4).max(limit).min(100);
    let bm25_candidates = query_bm25_candidates(db, opts, query, pool_limit)
        .context("failed to query BM25 knowledge candidates")?;
    let query_vec_sql = format_query_vec_sql(opts.query_vec.as_deref());
    let vector_candidates = match query_vec_sql {
        Some(ref query_vec_sql) if !matches!(opts.scope, KnowledgeScope::Docs) => {
            query_vector_candidates(db, opts, query_vec_sql, pool_limit)
                .context("failed to query vector knowledge candidates")?
        }
        _ => Vec::new(),
    };

    let ranked = merge_candidates(bm25_candidates, vector_candidates, limit);
    let total_code = ranked.iter().filter(|candidate| is_code(candidate)).count();
    let total_docs = ranked.iter().filter(|candidate| is_doc(candidate)).count();
    let (primary_evidence, supporting_docs) = split_evidence(&ranked, opts);
    let answerable = !primary_evidence.is_empty() || !supporting_docs.is_empty();
    let confidence = confidence_for_result(&ranked, primary_evidence.len() + supporting_docs.len());

    Ok(KnowledgeContextResult {
        primary_evidence,
        supporting_docs,
        confidence,
        answerable,
        graph_content_hash: query_graph_content_hash(db, opts),
        candidates: KnowledgeCandidateSummary {
            total: ranked.len(),
            returned_primary: total_code,
            returned_supporting_docs: total_docs,
            total_code,
            total_docs,
        },
    })
}

fn query_bm25_candidates(
    db: &Connection,
    opts: &KnowledgeContextOptions,
    query: &str,
    limit: usize,
) -> Result<Vec<KnowledgeCandidate>> {
    let mut stmt = db
        .prepare(
            r"
            WITH corpus AS (
                SELECT
                    'doc:' || section_id AS candidate_key,
                    'doc' AS kind,
                    COALESCE(title, section_id) AS title,
                    file_path,
                    section_id AS stable_symbol_id,
                    CAST(NULL AS VARCHAR) AS qualified_name,
                    CAST('section' AS VARCHAR) AS symbol_kind,
                    COALESCE(title, '') || ' ' || COALESCE(body_text, '') AS search_text
                FROM section_bodies
                WHERE source = $2
                  AND package = $3
                  AND revision = $4
                  AND $5 IN ('all', 'docs')
                UNION ALL
                SELECT
                    'code:' || stable_symbol_id AS candidate_key,
                    'code' AS kind,
                    COALESCE(entity_name, qualified_name, stable_symbol_id) AS title,
                    file_path,
                    stable_symbol_id,
                    qualified_name,
                    symbol_kind,
                    COALESCE(entity_name, '') || ' ' || COALESCE(qualified_name, '') AS search_text
                FROM nodes
                WHERE source = $2
                  AND package = $3
                  AND revision = $4
                  AND $5 IN ('all', 'code')
                  AND COALESCE(symbol_kind, '') NOT IN ('section', 'mcp_tool')
            ),
            query_terms AS (
                SELECT DISTINCT term
                FROM UNNEST(regexp_extract_all(lower($1), '[[:alnum:]]+')) AS terms(term)
                WHERE term <> ''
            ),
            tokens AS (
                SELECT c.candidate_key, token
                FROM corpus c,
                     UNNEST(regexp_extract_all(lower(c.search_text), '[[:alnum:]]+')) AS toks(token)
                WHERE token <> ''
            ),
            lengths AS (
                SELECT candidate_key, COUNT(*)::DOUBLE AS doc_len
                FROM tokens
                GROUP BY candidate_key
            ),
            term_freq AS (
                SELECT t.candidate_key, t.token AS term, COUNT(*)::DOUBLE AS tf
                FROM tokens t
                JOIN query_terms q ON q.term = t.token
                GROUP BY t.candidate_key, t.token
            ),
            doc_freq AS (
                SELECT term, COUNT(DISTINCT candidate_key)::DOUBLE AS df
                FROM term_freq
                GROUP BY term
            ),
            stats AS (
                SELECT COUNT(*)::DOUBLE AS n_docs,
                       COALESCE(AVG(doc_len), 0.0) AS avgdl
                FROM lengths
            ),
            scored AS (
                SELECT
                    c.kind,
                    c.title,
                    c.file_path,
                    c.stable_symbol_id,
                    c.qualified_name,
                    c.symbol_kind,
                    SUM(
                        ln(1.0 + ((s.n_docs - df.df + 0.5) / (df.df + 0.5)))
                        * ((tf.tf * 2.2)
                           / (tf.tf + 1.2 * (0.25 + 0.75 * (l.doc_len / NULLIF(s.avgdl, 0.0)))))
                    ) AS raw_score
                FROM term_freq tf
                JOIN doc_freq df ON df.term = tf.term
                JOIN lengths l ON l.candidate_key = tf.candidate_key
                JOIN corpus c ON c.candidate_key = tf.candidate_key
                CROSS JOIN stats s
                GROUP BY c.kind, c.title, c.file_path, c.stable_symbol_id,
                         c.qualified_name, c.symbol_kind
            )
            SELECT
                kind,
                title,
                file_path,
                stable_symbol_id,
                qualified_name,
                symbol_kind,
                round(
                    raw_score
                    * CASE
                        WHEN kind = 'code'
                         AND symbol_kind IN ('function', 'method', 'struct', 'enum', 'trait')
                        THEN 1.15
                        WHEN kind = 'code'
                         AND symbol_kind IN ('constant', 'static', 'field')
                        THEN 0.85
                        ELSE 1.0
                      END,
                    3
                ) AS score,
                CASE WHEN kind = 'code' THEN 'primary' ELSE NULL END AS neighbor_kind,
                CASE WHEN kind = 'code' THEN 'bm25-code' ELSE 'bm25-doc' END AS grounding
            FROM scored
            WHERE raw_score IS NOT NULL
            ORDER BY score DESC NULLS LAST, file_path, title, stable_symbol_id
            LIMIT $6
            ",
        )
        .context("failed to prepare BM25 knowledge query")?;
    let rows = stmt
        .query_map(
            params![
                query,
                opts.source,
                opts.package,
                opts.revision,
                opts.scope.as_sql_scope(),
                limit as i64
            ],
            candidate_from_bm25_row,
        )
        .context("failed to run BM25 knowledge query")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read BM25 knowledge rows")
}

fn query_vector_candidates(
    db: &Connection,
    opts: &KnowledgeContextOptions,
    query_vec_sql: &str,
    limit: usize,
) -> Result<Vec<KnowledgeCandidate>> {
    let sql = format!(
        r"
        WITH distances AS (
            SELECT
                entity_name AS title,
                file_path,
                stable_symbol_id,
                qualified_name,
                symbol_kind,
                array_cosine_distance(embedding, {query_vec_sql}) AS distance
            FROM symbol_embeddings
            WHERE source = $1
              AND package = $2
              AND revision = $3
        )
        SELECT
            'code' AS kind,
            title,
            file_path,
            stable_symbol_id,
            qualified_name,
            symbol_kind,
            round(GREATEST(0.0, 1.0 - distance), 6) AS score,
            'primary' AS neighbor_kind,
            'hybrid-code' AS grounding
        FROM distances
        WHERE distance IS NOT NULL
        ORDER BY distance ASC NULLS LAST, file_path, title, stable_symbol_id
        LIMIT $4
        "
    );
    let mut stmt = db
        .prepare(&sql)
        .context("failed to prepare vector knowledge query")?;
    let rows = stmt
        .query_map(
            params![opts.source, opts.package, opts.revision, limit as i64],
            candidate_from_vector_row,
        )
        .context("failed to run vector knowledge query")?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read vector knowledge rows")
}

fn candidate_from_bm25_row(row: &Row<'_>) -> duckdb::Result<KnowledgeCandidate> {
    let kind: String = row.get(0)?;
    Ok(KnowledgeCandidate {
        kind,
        title: row.get(1)?,
        file_path: row.get(2)?,
        stable_symbol_id: row.get(3)?,
        qualified_name: row.get(4)?,
        symbol_kind: row.get(5)?,
        score: row.get(6)?,
        signal: None,
        neighbor_kind: row.get(7)?,
        edge_bind_method: None,
        grounding: row.get(8)?,
    })
}

fn candidate_from_vector_row(row: &Row<'_>) -> duckdb::Result<KnowledgeCandidate> {
    Ok(KnowledgeCandidate {
        kind: row.get(0)?,
        title: row.get(1)?,
        file_path: row.get(2)?,
        stable_symbol_id: row.get(3)?,
        qualified_name: row.get(4)?,
        symbol_kind: row.get(5)?,
        score: row.get(6)?,
        signal: None,
        neighbor_kind: row.get(7)?,
        edge_bind_method: None,
        grounding: row.get(8)?,
    })
}

fn merge_candidates(
    mut bm25_candidates: Vec<KnowledgeCandidate>,
    mut vector_candidates: Vec<KnowledgeCandidate>,
    limit: usize,
) -> Vec<KnowledgeCandidate> {
    sort_candidates(&mut bm25_candidates);
    sort_candidates(&mut vector_candidates);

    if vector_candidates.is_empty() {
        bm25_candidates.truncate(limit);
        return bm25_candidates;
    }
    if bm25_candidates.is_empty() {
        vector_candidates.truncate(limit);
        return vector_candidates;
    }

    let mut entries: HashMap<String, MergeEntry> = HashMap::new();
    add_ranked_candidates(&mut entries, bm25_candidates, 0);
    add_ranked_candidates(&mut entries, vector_candidates, 1);

    let mut merged = entries
        .into_values()
        .map(|entry| {
            let mut candidate = entry.candidate;
            candidate.score = (entry.fused_score / (2.0 / 61.0)).min(1.0);
            if entry.has_vector && is_code(&candidate) {
                candidate.grounding = "hybrid-code".to_owned();
            }
            (candidate, entry.best_rank, entry.best_priority)
        })
        .collect::<Vec<_>>();

    merged.sort_by(
        |(left, left_rank, left_priority), (right, right_rank, right_priority)| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left_rank.cmp(right_rank))
                .then_with(|| left_priority.cmp(right_priority))
                .then_with(|| left.file_path.cmp(&right.file_path))
                .then_with(|| left.title.cmp(&right.title))
        },
    );
    merged
        .into_iter()
        .map(|(candidate, _, _)| candidate)
        .take(limit)
        .collect()
}

#[derive(Debug)]
struct MergeEntry {
    candidate: KnowledgeCandidate,
    fused_score: f64,
    best_rank: usize,
    best_priority: usize,
    has_vector: bool,
}

fn add_ranked_candidates(
    entries: &mut HashMap<String, MergeEntry>,
    candidates: Vec<KnowledgeCandidate>,
    priority: usize,
) {
    for (index, candidate) in candidates.into_iter().enumerate() {
        let rank = index + 1;
        let candidate_id = candidate_id(&candidate);
        let contribution = 1.0 / (60.0 + rank as f64);
        let has_vector = candidate.grounding.starts_with("hybrid-");
        entries
            .entry(candidate_id)
            .and_modify(|entry| {
                entry.fused_score += contribution;
                entry.has_vector |= has_vector;
                if rank < entry.best_rank
                    || (rank == entry.best_rank && priority < entry.best_priority)
                {
                    entry.candidate = candidate.clone();
                    entry.best_rank = rank;
                    entry.best_priority = priority;
                }
            })
            .or_insert(MergeEntry {
                candidate,
                fused_score: contribution,
                best_rank: rank,
                best_priority: priority,
                has_vector,
            });
    }
}

fn sort_candidates(candidates: &mut [KnowledgeCandidate]) {
    candidates.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.file_path.cmp(&right.file_path))
            .then_with(|| left.title.cmp(&right.title))
            .then_with(|| left.stable_symbol_id.cmp(&right.stable_symbol_id))
    });
}

fn candidate_id(candidate: &KnowledgeCandidate) -> String {
    candidate.stable_symbol_id.clone().unwrap_or_else(|| {
        format!(
            "{}:{}:{}",
            candidate.kind, candidate.file_path, candidate.title
        )
    })
}

fn split_evidence(
    candidates: &[KnowledgeCandidate],
    opts: &KnowledgeContextOptions,
) -> (Vec<KnowledgeEvidence>, Vec<KnowledgeEvidence>) {
    let mut primary = Vec::new();
    let mut docs = Vec::new();
    for candidate in candidates {
        let evidence = evidence_from_candidate(candidate, opts);
        if is_doc(candidate) {
            docs.push(evidence);
        } else {
            primary.push(evidence);
        }
    }
    (primary, docs)
}

fn evidence_from_candidate(
    candidate: &KnowledgeCandidate,
    opts: &KnowledgeContextOptions,
) -> KnowledgeEvidence {
    let stable_symbol_id = if is_code(candidate) {
        candidate.qualified_name.as_ref().map(|qualified_name| {
            format!("pkg:{}@{}::{}", opts.package, opts.revision, qualified_name)
        })
    } else {
        candidate.stable_symbol_id.clone()
    };
    let next = if is_code(candidate) {
        stable_symbol_id
            .as_ref()
            .map(|selector| {
                vec![
                    json!({ "tool": "external_code_read", "selector": selector }),
                    json!({ "tool": "external_code_callers", "selector": selector }),
                    json!({ "tool": "external_code_callees", "selector": selector }),
                ]
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    KnowledgeEvidence {
        kind: if is_code(candidate) {
            "symbol".to_owned()
        } else {
            "doc".to_owned()
        },
        title: candidate.title.clone(),
        file: candidate.file_path.clone(),
        stable_symbol_id,
        symbol_kind: candidate.symbol_kind.clone(),
        score: candidate.score,
        signal: candidate.signal.clone(),
        neighbor_kind: candidate.neighbor_kind.clone(),
        edge_bind_method: candidate.edge_bind_method.clone(),
        grounding: candidate.grounding.clone(),
        why_relevant: build_why_relevant(candidate),
        next,
    }
}

fn build_why_relevant(candidate: &KnowledgeCandidate) -> String {
    let mut parts = vec![format!(
        "{} {:.1}",
        grounding_score_prefix(&candidate.grounding),
        candidate.score
    )];
    if let Some(signal) = &candidate.signal {
        parts.push(signal.clone());
    }
    if let Some(kind) = &candidate.symbol_kind {
        parts.push(format!("kind={kind}"));
    }
    parts.push(format!("grounding={}", candidate.grounding));
    parts.join(", ")
}

fn grounding_score_prefix(grounding: &str) -> &str {
    match grounding {
        "bm25-code" | "bm25-doc" => "BM25",
        "hybrid-code" => "hybrid",
        _ if grounding.starts_with("bm25-") => "BM25",
        _ => grounding,
    }
}

fn confidence_for_result(candidates: &[KnowledgeCandidate], evidence_count: usize) -> String {
    let Some(top) = candidates.first() else {
        return "low".to_owned();
    };
    let (high_score, medium_score) = confidence_score_thresholds(&top.grounding);
    if top.score > high_score && evidence_count >= 3 {
        "high".to_owned()
    } else if top.score > medium_score && evidence_count >= 2 {
        "medium".to_owned()
    } else {
        "low".to_owned()
    }
}

fn confidence_score_thresholds(grounding: &str) -> (f64, f64) {
    if grounding.starts_with("hybrid-") {
        (HYBRID_HIGH_CONFIDENCE_SCORE, HYBRID_MEDIUM_CONFIDENCE_SCORE)
    } else {
        (BM25_HIGH_CONFIDENCE_SCORE, BM25_MEDIUM_CONFIDENCE_SCORE)
    }
}

fn query_graph_content_hash(db: &Connection, opts: &KnowledgeContextOptions) -> Option<String> {
    db.query_row("SELECT graph_content_hash FROM _meta LIMIT 1", [], |row| {
        row.get(0)
    })
    .ok()
    .or_else(|| {
        db.query_row(
            r"
            SELECT graph_content_hash
            FROM package_catalog
            WHERE source = $1
              AND package = $2
              AND revision = $3
            LIMIT 1
            ",
            params![opts.source, opts.package, opts.revision],
            |row| row.get(0),
        )
        .ok()
    })
}

fn format_query_vec_sql(query_vec: Option<&[f32]>) -> Option<String> {
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

fn is_code(candidate: &KnowledgeCandidate) -> bool {
    candidate.kind == "code" || candidate.kind == "symbol"
}

fn is_doc(candidate: &KnowledgeCandidate) -> bool {
    candidate.kind == "doc"
}
