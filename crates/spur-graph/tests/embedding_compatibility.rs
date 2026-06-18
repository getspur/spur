use spur_graph::{
    EmbeddingModelSelection, EMBEDDING_VECTOR_DIMENSIONS, EMBED_MODEL_ENV, EMBED_MODEL_NAME,
    JINA_CODE_EMBED_MODEL_NAME,
};

#[test]
fn embedding_contract_uses_bge_base_model_and_dimensions() {
    assert_eq!(EMBED_MODEL_NAME, "BGEBaseENV15");
    assert_eq!(EMBEDDING_VECTOR_DIMENSIONS, 768);
}

#[test]
fn embedding_contract_supports_jina_code_without_dimension_change() {
    assert_eq!(EMBED_MODEL_ENV, "SPUR_EMBEDDING_MODEL");
    assert_eq!(JINA_CODE_EMBED_MODEL_NAME, "JinaEmbeddingsV2BaseCode");
    assert_eq!(
        EmbeddingModelSelection::parse("jina-code"),
        Some(EmbeddingModelSelection::JinaCode)
    );
    assert_eq!(EmbeddingModelSelection::JinaCode.dimensions(), 768);
}
