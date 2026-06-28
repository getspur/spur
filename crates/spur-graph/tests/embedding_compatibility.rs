use spur_graph::{
    store::lance_sections::{CODE_SYMBOL_EMBED_SKIP_ENV, SECTION_EMBED_SKIP_ENV},
    EmbeddingModelSelection, EMBEDDING_GEMMA_EMBED_MODEL_NAME, EMBEDDING_VECTOR_DIMENSIONS,
    EMBED_MODEL_ENV,
};

#[test]
fn embedding_contract_uses_gemma_model_and_dimensions() {
    assert_eq!(EMBEDDING_GEMMA_EMBED_MODEL_NAME, "EmbeddingGemma300M");
    assert_eq!(EMBEDDING_VECTOR_DIMENSIONS, 768);
}

#[test]
fn embedding_contract_rejects_legacy_model_aliases() {
    assert_eq!(EMBED_MODEL_ENV, "SPUR_EMBEDDING_MODEL");
    assert_eq!(EmbeddingModelSelection::parse("jina-code"), None);
    assert_eq!(EmbeddingModelSelection::parse("bge-base-en-v1.5"), None);
    assert_eq!(
        EmbeddingModelSelection::parse("google/embeddinggemma-300m"),
        Some(EmbeddingModelSelection::EmbeddingGemma300M)
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
