//! Integration tests for policy signing.
//!
//! These verify that the admin signing tooling produces `SignedPolicy`
//! artifacts that pass cryptographic verification.

use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier as _};

/// Helper: generate a deterministic test key from a seed.
fn test_signing_key() -> ed25519_dalek::SigningKey {
    let seed = [0x42u8; 32];
    ed25519_dalek::SigningKey::from_bytes(&seed)
}

#[test]
fn sign_policy_produces_valid_signature() {
    let payload = r#"{"schema_version":1,"issued_at":"2026-04-27T00:00:00Z","tier_policies":{}}"#;
    let signing_key = test_signing_key();

    // Expected API: sign_policy(payload, key_id, signing_key) -> SignedPolicy
    let signed = spur_license_admin::policy_sign::sign_policy(payload, "test-key", &signing_key);

    assert_eq!(signed.key_id, "test-key");
    assert_eq!(signed.payload, payload);

    // Cryptographic verification
    let verifying_key = signing_key.verifying_key();
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&signed.signature)
        .expect("signature must be valid base64");
    let signature = Signature::from_slice(&sig_bytes).expect("signature must be 64 bytes");
    verifying_key
        .verify(payload.as_bytes(), &signature)
        .expect("signature must verify against payload");
}

#[test]
fn sign_policy_payload_is_preserved_exactly() {
    let payload = r#"{"schema_version":1,"issued_at":"2026-04-27T00:00:00Z","tier_policies":{}}"#;
    let signing_key = test_signing_key();

    let signed = spur_license_admin::policy_sign::sign_policy(payload, "test-key", &signing_key);

    assert_eq!(signed.payload, payload, "payload must be preserved exactly");
}
