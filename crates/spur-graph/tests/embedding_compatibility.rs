use spur_graph::{
    embedding_query_text_for_model,
    store::lance_sections::{CODE_SYMBOL_EMBED_SKIP_ENV, SECTION_EMBED_SKIP_ENV},
    EmbeddingModelSelection, CODE_RANK_EMBED_MODEL_NAME, EMBEDDING_VECTOR_DIMENSIONS,
    EMBED_MODEL_ENV, JINA_EMBEDDINGS_V2_BASE_CODE_MODEL_NAME, NOMIC_EMBED_TEXT_V15_MODEL_NAME,
};

#[test]
fn embedding_contract_supports_nomic_v15_fp32_at_768_dimensions() {
    let model = EmbeddingModelSelection::parse("nomic-ai/nomic-embed-text-v1.5")
        .expect("Nomic v1.5 should be selectable");

    assert_eq!(model.model_name(), "NomicEmbedTextV15");
    assert_eq!(model.dimensions(), 768);
    assert_eq!(
        embedding_query_text_for_model("find task spawner", model),
        "search_query: find task spawner"
    );
}

#[test]
fn embedding_contract_uses_nomic_model_and_dimensions() {
    assert_eq!(NOMIC_EMBED_TEXT_V15_MODEL_NAME, "NomicEmbedTextV15");
    assert_eq!(EMBEDDING_VECTOR_DIMENSIONS, 768);
}

#[test]
fn embedding_contract_supports_optional_code_models_at_768_dimensions() {
    let coderank = EmbeddingModelSelection::parse("nomic-ai/CodeRankEmbed")
        .expect("CodeRankEmbed should be selectable");
    let jina = EmbeddingModelSelection::parse("jinaai/jina-embeddings-v2-base-code")
        .expect("Jina code should be selectable");

    assert_eq!(coderank.model_name(), CODE_RANK_EMBED_MODEL_NAME);
    assert_eq!(jina.model_name(), JINA_EMBEDDINGS_V2_BASE_CODE_MODEL_NAME);
    assert_eq!(coderank.dimensions(), EMBEDDING_VECTOR_DIMENSIONS);
    assert_eq!(jina.dimensions(), EMBEDDING_VECTOR_DIMENSIONS);
    assert_eq!(
        embedding_query_text_for_model("find task spawner", coderank),
        "Represent this query for searching relevant code: find task spawner"
    );
    assert_eq!(
        embedding_query_text_for_model("find task spawner", jina),
        "find task spawner"
    );
}

#[test]
fn embedding_contract_rejects_legacy_model_aliases() {
    assert_eq!(EMBED_MODEL_ENV, "SPUR_EMBEDDING_MODEL");
    assert_eq!(EmbeddingModelSelection::parse("bge-base-en-v1.5"), None);
    assert_eq!(
        EmbeddingModelSelection::parse("google/embeddinggemma-300m"),
        None
    );
    assert_eq!(
        EmbeddingModelSelection::parse("nomic-ai/nomic-embed-text-v1.5"),
        Some(EmbeddingModelSelection::NomicEmbedTextV15)
    );
    assert_eq!(
        EmbeddingModelSelection::parse("nomic-coderank"),
        Some(EmbeddingModelSelection::CodeRankEmbed)
    );
    assert_eq!(
        EmbeddingModelSelection::parse("jina-code"),
        Some(EmbeddingModelSelection::JinaEmbeddingsV2BaseCode)
    );
}

#[test]
fn embedding_skip_env_contract_splits_section_and_code_symbol_flags() {
    assert_eq!(SECTION_EMBED_SKIP_ENV, "SPUR_GRAPH_SKIP_SECTION_EMBEDDINGS");
    assert_eq!(
        CODE_SYMBOL_EMBED_SKIP_ENV,
        "SPUR_GRAPH_SKIP_CODE_SYMBOL_EMBEDDINGS"
    );
}
