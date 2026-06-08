use spur_graph::{EMBEDDING_VECTOR_DIMENSIONS, EMBED_MODEL_NAME};

#[test]
fn embedding_contract_uses_bge_base_model_and_dimensions() {
    assert_eq!(EMBED_MODEL_NAME, "BGEBaseENV15");
    assert_eq!(EMBEDDING_VECTOR_DIMENSIONS, 768);
}
