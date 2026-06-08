use spur_graph::embedding::openrouter::OpenRouterEmbedder;

#[test]
fn openrouter_embedder_uses_bge_base_batch_contract() {
    assert_eq!(OpenRouterEmbedder::MODEL, "baai/bge-base-en-v1.5");
    assert_eq!(OpenRouterEmbedder::BATCH_SIZE, 256);
}
