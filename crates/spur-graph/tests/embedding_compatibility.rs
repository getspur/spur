use spur_graph::{
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
