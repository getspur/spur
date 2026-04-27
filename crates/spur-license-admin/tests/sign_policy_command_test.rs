//! Integration tests for the `sign-policy` CLI command.

use std::fs;
use std::path::PathBuf;

use ed25519_dalek::{Signature, Verifier};
use spur_license::policy::SignedPolicy;

fn test_signing_key_path(temp_dir: &std::path::Path) -> PathBuf {
    let seed = [0x42u8; 32];
    let path = temp_dir.join("test-key.raw");
    fs::write(&path, seed).expect("write test key");
    path
}

#[tokio::test]
async fn sign_policy_command_reads_input_and_writes_signed_output() {
    let temp_dir =
        std::env::temp_dir().join(format!("spur-license-admin-test-{}", std::process::id()));
    fs::create_dir_all(&temp_dir).expect("create temp dir");
    let input_path = temp_dir.join("policy.json");
    let output_path = temp_dir.join("signed.json");
    let key_path = test_signing_key_path(&temp_dir);

    let payload = r#"{"schema_version":1,"issued_at":"2026-04-27T00:00:00Z","tier_policies":{}}"#;
    fs::write(&input_path, payload).expect("write input");

    spur_license_admin::commands::sign_policy::run(
        &input_path,
        Some(&output_path),
        "test-key",
        &key_path,
    )
    .await
    .expect("sign-policy command should succeed");

    let raw = fs::read_to_string(&output_path).expect("read output");
    let signed: SignedPolicy =
        serde_json::from_str(&raw).expect("output must be valid SignedPolicy");

    assert_eq!(signed.key_id, "test-key");
    assert_eq!(signed.payload, payload);

    let signing_key = ed25519_dalek::SigningKey::from_bytes(
        &fs::read(&key_path)
            .expect("read key")
            .try_into()
            .expect("32 bytes"),
    );
    let verifying_key = signing_key.verifying_key();

    use base64::Engine as _;
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&signed.signature)
        .expect("valid base64");
    let signature = Signature::from_slice(&sig_bytes).expect("valid signature");
    verifying_key
        .verify(payload.as_bytes(), &signature)
        .expect("signature must verify");

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
}
