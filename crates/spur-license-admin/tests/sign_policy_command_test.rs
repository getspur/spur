//! Integration tests for the `sign-policy` CLI command.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use base64::Engine as _;
use ed25519_dalek::pkcs8::spki::der::pem::LineEnding;
use ed25519_dalek::pkcs8::EncodePrivateKey as _;
use ed25519_dalek::{Signature, Verifier as _};
use spur_license::policy::SignedPolicy;

const TEST_SEED: [u8; 32] = [0x42u8; 32];

fn unique_temp_dir(label: &str) -> std::path::PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "spur-license-admin-{label}-{}-{n}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn test_raw_key_path(temp_dir: &std::path::Path) -> PathBuf {
    let path = temp_dir.join("test-key.raw");
    fs::write(&path, TEST_SEED).expect("write test key");
    path
}

#[tokio::test]
async fn sign_policy_command_reads_input_and_writes_signed_output() {
    let temp_dir = unique_temp_dir("raw");
    let input_path = temp_dir.join("policy.json");
    let output_path = temp_dir.join("signed.json");
    let key_path = test_raw_key_path(&temp_dir);

    let payload = r#"{"schema_version":1,"issued_at":"2026-04-27T00:00:00Z","tier_policies":{}}"#;
    fs::write(&input_path, payload).expect("write input");

    spur_license_admin::commands::sign_policy::run(
        &input_path,
        Some(&output_path),
        "test-key",
        &key_path,
    )
    .expect("sign-policy command should succeed");

    let raw = fs::read_to_string(&output_path).expect("read output");
    let signed: SignedPolicy =
        serde_json::from_str(&raw).expect("output must be valid SignedPolicy");

    assert_eq!(signed.key_id, "test-key");
    assert_eq!(signed.payload, payload);

    // Explicit self-verify positive path: the produced signature passes
    // verifying_key().verify(payload.as_bytes(), &sig). A future regression
    // that breaks the in-command self-verify would also break this assertion.
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&TEST_SEED);
    let verifying_key = signing_key.verifying_key();

    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&signed.signature)
        .expect("valid base64");
    let signature = Signature::from_slice(&sig_bytes).expect("valid signature");
    verifying_key
        .verify(payload.as_bytes(), &signature)
        .expect("signature must verify");

    let _ = fs::remove_dir_all(&temp_dir);
}

/// Exercises the PEM-fallback branch in `load_signing_key`: when the key file
/// is not exactly 32 bytes, the loader must parse it as a PKCS#8 PEM file.
#[tokio::test]
async fn sign_policy_command_accepts_pkcs8_pem_signing_key() {
    let temp_dir = unique_temp_dir("pem");
    let input_path = temp_dir.join("policy.json");
    let output_path = temp_dir.join("signed.json");
    let key_path = temp_dir.join("test-key.pem");

    // Emit a deterministic PKCS#8 PEM from the same seed used elsewhere so
    // the produced signature is reproducible across runs.
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&TEST_SEED);
    let pem = signing_key
        .to_pkcs8_pem(LineEnding::LF)
        .expect("encode pkcs8 pem");
    fs::write(&key_path, pem.as_bytes()).expect("write pem");

    // Sanity-check we are actually exercising the PEM branch (length != 32).
    let key_len = fs::metadata(&key_path).expect("stat pem").len();
    assert_ne!(
        key_len, 32,
        "test setup error: PEM file must not be 32 bytes; got {key_len}"
    );

    let payload = r#"{"schema_version":1,"issued_at":"2026-04-27T00:00:00Z","tier_policies":{}}"#;
    fs::write(&input_path, payload).expect("write input");

    spur_license_admin::commands::sign_policy::run(
        &input_path,
        Some(&output_path),
        "test-key-pem",
        &key_path,
    )
    .expect("sign-policy command must succeed with a PKCS#8 PEM key");

    let raw = fs::read_to_string(&output_path).expect("read output");
    let signed: SignedPolicy =
        serde_json::from_str(&raw).expect("output must be valid SignedPolicy");

    assert_eq!(signed.key_id, "test-key-pem");
    assert_eq!(signed.payload, payload);

    // Verify externally with the corresponding verifying_key.
    let verifying_key = signing_key.verifying_key();
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&signed.signature)
        .expect("valid base64");
    let signature = Signature::from_slice(&sig_bytes).expect("valid signature");
    verifying_key
        .verify(payload.as_bytes(), &signature)
        .expect("signature produced via PEM path must verify");

    let _ = fs::remove_dir_all(&temp_dir);
}
