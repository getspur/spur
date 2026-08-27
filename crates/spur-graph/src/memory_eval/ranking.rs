//! Leakage-safe ranking contracts and implementations.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The shared normalization, BM25, and tie-breaking contract.
///
/// This value is included in [`Ranking::serialization_sha256`] so a change to
/// tokenization or scoring cannot be mistaken for the same experiment.
pub const TOKENIZATION_CONTRACT: &str =
    "unicode-alphanumeric-lowercase-v1;bm25-k1=1.2;bm25-b=0.75;tie=occurrence-id-asc";

const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

/// Caller-owned query identity for externally keyed ranking artifacts.
///
/// Rank execution never receives or produces this value. Callers attach it to
/// an unkeyed [`Ranking`] only after the ranker returns.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct QueryOccurrenceId(String);

impl QueryOccurrenceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

/// A dataset-adapter chronology coordinate, ordered from older to newer.
///
/// Adapters normalize source dates and timestamps into this numeric key before
/// constructing the question-blind corpus. The ranker never compares raw date
/// strings, and the key is part of the corpus serialization hash.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct ChronologyKey(i64);

impl ChronologyKey {
    pub const fn new(value: i64) -> Self {
        Self(value)
    }
}

/// A controlled benchmark retrieval variant.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Variant {
    Oracle,
    Recent,
    FlatBm25,
    GraphIndexOnly,
    GraphTraversal,
}

/// The canonical provenance unit counted by a ranking budget.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    Turn,
    Session,
}

/// A question-blind corpus view available to every non-oracle ranker.
///
/// It intentionally contains neither source question records nor any answer,
/// evidence, question-type, or `has_answer` fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CorpusDocument {
    /// Occurrence-scoped canonical provenance, never a source gold identifier.
    pub occurrence_id: String,
    /// Shared text serialization used by every lexical variant.
    pub text: String,
    /// Normalized source chronology used only by the recent baseline.
    pub chronology_key: Option<ChronologyKey>,
}

/// The complete input that a non-oracle ranker is allowed to receive.
#[derive(Debug, Clone, Serialize)]
pub struct RankRequest<'a> {
    pub query: &'a str,
    pub granularity: Granularity,
    pub corpus: &'a [CorpusDocument],
}

/// Scorer-only gold access for the oracle control.
///
/// Non-oracle implementations accept [`RankRequest`] directly and therefore
/// cannot receive this type by accident.
#[derive(Debug, Clone)]
pub struct OracleRequest<'a> {
    pub request: RankRequest<'a>,
    pub gold_occurrence_ids: &'a [String],
}

/// One ranked canonical provenance occurrence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RankedHit {
    pub occurrence_id: String,
    /// Graph variants may record an internal-to-canonical provenance mapping.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance_id: Option<String>,
    pub score: f64,
}

/// An unkeyed immutable ranking artifact for exactly one granularity and cutoff.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Ranking {
    pub variant: Variant,
    pub granularity: Granularity,
    pub k: usize,
    pub hits: Vec<RankedHit>,
    pub query_sha256: String,
    pub corpus_sha256: String,
    pub serialization_sha256: String,
}

/// Rankings are keyed separately by question, variant, and granularity.
pub type RankingSet = BTreeMap<(QueryOccurrenceId, Variant, Granularity), Ranking>;

/// A non-oracle retrieval implementation.
pub trait Ranker {
    fn variant(&self) -> Variant;

    fn rank(&self, request: &RankRequest<'_>, k: usize) -> Result<Ranking>;
}

/// Deterministic newest-first chronological baseline.
#[derive(Debug, Clone, Copy, Default)]
pub struct RecentRanker;

impl Ranker for RecentRanker {
    fn variant(&self) -> Variant {
        Variant::Recent
    }

    fn rank(&self, request: &RankRequest<'_>, k: usize) -> Result<Ranking> {
        let mut candidates = request
            .corpus
            .iter()
            .map(|document| (document.occurrence_id.as_str(), document.chronology_key))
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));

        let hits = unique_top_k(
            candidates.into_iter().map(|(occurrence_id, _)| RankedHit {
                occurrence_id: occurrence_id.to_owned(),
                provenance_id: None,
                score: 0.0,
            }),
            k,
        );
        ranking(request, self.variant(), k, hits)
    }
}

/// A BM25 index over the exact shared corpus view.
#[derive(Debug, Clone)]
pub struct Bm25Ranker {
    corpus: Vec<CorpusDocument>,
    documents: Vec<IndexedDocument>,
    document_frequency: BTreeMap<String, usize>,
    average_document_length: f64,
}

#[derive(Debug, Clone)]
struct IndexedDocument {
    occurrence_id: String,
    term_frequency: BTreeMap<String, usize>,
    length: usize,
}

impl Bm25Ranker {
    /// Build an index from the same leakage-safe corpus later carried by the
    /// rank request.
    pub fn build(corpus: Vec<CorpusDocument>) -> Result<Self> {
        ensure!(
            corpus
                .iter()
                .all(|document| !document.occurrence_id.is_empty()),
            "corpus occurrence IDs must not be empty"
        );

        let mut document_frequency = BTreeMap::<String, usize>::new();
        let mut documents = Vec::with_capacity(corpus.len());
        let mut total_document_length = 0usize;

        for document in &corpus {
            let tokens = normalized_tokens(&document.text);
            let mut term_frequency = BTreeMap::<String, usize>::new();
            for token in tokens {
                *term_frequency.entry(token).or_default() += 1;
            }
            for token in term_frequency.keys() {
                *document_frequency.entry(token.clone()).or_default() += 1;
            }

            let length = term_frequency.values().sum();
            total_document_length += length;
            documents.push(IndexedDocument {
                occurrence_id: document.occurrence_id.clone(),
                term_frequency,
                length,
            });
        }

        let average_document_length = if documents.is_empty() {
            0.0
        } else {
            total_document_length as f64 / documents.len() as f64
        };

        Ok(Self {
            corpus,
            documents,
            document_frequency,
            average_document_length,
        })
    }

    fn score(&self, document: &IndexedDocument, query_tokens: &[String]) -> f64 {
        if self.documents.is_empty() || self.average_document_length == 0.0 {
            return 0.0;
        }

        let document_count = self.documents.len() as f64;
        query_tokens
            .iter()
            .filter_map(|token| {
                let term_frequency = *document.term_frequency.get(token)? as f64;
                let document_frequency = *self.document_frequency.get(token)? as f64;
                let inverse_document_frequency = (1.0
                    + (document_count - document_frequency + 0.5) / (document_frequency + 0.5))
                    .ln();
                let length_normalization =
                    1.0 - BM25_B + BM25_B * document.length as f64 / self.average_document_length;
                let saturated_frequency = term_frequency * (BM25_K1 + 1.0)
                    / (term_frequency + BM25_K1 * length_normalization);
                Some(inverse_document_frequency * saturated_frequency)
            })
            .sum()
    }
}

impl Ranker for Bm25Ranker {
    fn variant(&self) -> Variant {
        Variant::FlatBm25
    }

    fn rank(&self, request: &RankRequest<'_>, k: usize) -> Result<Ranking> {
        ensure!(
            request.corpus == self.corpus.as_slice(),
            "rank request corpus does not match the BM25 build corpus"
        );

        let query_tokens = normalized_tokens(request.query);
        let mut candidates = self
            .documents
            .iter()
            .map(|document| RankedHit {
                occurrence_id: document.occurrence_id.clone(),
                provenance_id: None,
                score: self.score(document, &query_tokens),
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.occurrence_id.cmp(&right.occurrence_id))
        });

        ranking(request, self.variant(), k, unique_top_k(candidates, k))
    }
}

/// Produce the scorer-only oracle control without exposing gold to a ranker.
pub fn oracle_ranking(request: &OracleRequest<'_>, k: usize) -> Ranking {
    let corpus_occurrences = request
        .request
        .corpus
        .iter()
        .map(|document| document.occurrence_id.as_str())
        .collect::<BTreeSet<_>>();
    let hits = unique_top_k(
        request
            .gold_occurrence_ids
            .iter()
            .filter(|occurrence_id| corpus_occurrences.contains(occurrence_id.as_str()))
            .map(|occurrence_id| RankedHit {
                occurrence_id: occurrence_id.clone(),
                provenance_id: None,
                score: 1.0,
            }),
        k,
    );

    ranking(&request.request, Variant::Oracle, k, hits)
        .expect("the fixed ranking serialization contains no fallible values")
}

fn unique_top_k(candidates: impl IntoIterator<Item = RankedHit>, k: usize) -> Vec<RankedHit> {
    let mut seen = BTreeSet::new();
    candidates
        .into_iter()
        .filter(|hit| seen.insert(hit.occurrence_id.clone()))
        .take(k)
        .collect()
}

fn ranking(
    request: &RankRequest<'_>,
    variant: Variant,
    k: usize,
    hits: Vec<RankedHit>,
) -> Result<Ranking> {
    let hashes = ranking_hashes(request)?;
    Ok(Ranking {
        variant,
        granularity: request.granularity,
        k,
        hits,
        query_sha256: hashes.query,
        corpus_sha256: hashes.corpus,
        serialization_sha256: hashes.serialization,
    })
}

struct RankingHashes {
    query: String,
    corpus: String,
    serialization: String,
}

#[derive(Serialize)]
struct TokenSerialization<'a> {
    contract: &'static str,
    query: Vec<String>,
    corpus: Vec<TokenizedDocument<'a>>,
}

#[derive(Serialize)]
struct TokenizedDocument<'a> {
    occurrence_id: &'a str,
    tokens: Vec<String>,
}

fn ranking_hashes(request: &RankRequest<'_>) -> Result<RankingHashes> {
    let corpus_serialization = serde_json::to_vec(request.corpus)?;
    let token_serialization = TokenSerialization {
        contract: TOKENIZATION_CONTRACT,
        query: normalized_tokens(request.query),
        corpus: request
            .corpus
            .iter()
            .map(|document| TokenizedDocument {
                occurrence_id: &document.occurrence_id,
                tokens: normalized_tokens(&document.text),
            })
            .collect(),
    };
    let token_serialization = serde_json::to_vec(&token_serialization)?;

    Ok(RankingHashes {
        query: sha256_hex(request.query.as_bytes()),
        corpus: sha256_hex(&corpus_serialization),
        serialization: sha256_hex(&token_serialization),
    })
}

fn normalized_tokens(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.chars().flat_map(char::to_lowercase).collect())
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
