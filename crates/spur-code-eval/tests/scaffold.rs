#[test]
fn exposes_versioned_contract_identity() {
    assert_eq!(spur_code_eval::CONTRACT_VERSION, "code-eval-v1");
}
