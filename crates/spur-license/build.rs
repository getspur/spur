//! Compile-time verifier for the embedded default policy.
//!
//! Loads `resources/default_policy.json`, parses it as `SignedPolicy`,
//! verifies the Ed25519 signature against the embedded public key, and
//! parses the inner `PolicyDocument`. Panics (= build failure) on any
//! error, so CI cannot ship a binary with a broken default policy.
//!
//! Re-runs only when the policy or key file changes.

use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use serde::Deserialize;

#[derive(Deserialize)]
struct SignedPolicy {
    payload: String,
    signature: String,
    key_id: String,
}

#[derive(Deserialize)]
struct PolicyDocumentMin {
    schema_version: u32,
}

const SUPPORTED_MAJOR: u32 = 2;

fn main() {
    println!("cargo:rerun-if-changed=resources/default_policy.json");
    println!("cargo:rerun-if-changed=resources/keys/spur-policy-2026-04.pub");

    let policy_raw = std::fs::read_to_string("resources/default_policy.json")
        .expect("resources/default_policy.json must exist");
    let signed: SignedPolicy =
        serde_json::from_str(&policy_raw).expect("default_policy.json must be a SignedPolicy JSON");

    assert!(
        signed.key_id == "spur-policy-2026-04",
        "embedded policy uses unknown key_id '{}'; expected 'spur-policy-2026-04'",
        signed.key_id
    );

    let key_bytes = std::fs::read("resources/keys/spur-policy-2026-04.pub")
        .expect("spur-policy-2026-04.pub must exist");
    let key_arr: [u8; 32] = key_bytes
        .as_slice()
        .try_into()
        .expect("public key must be exactly 32 bytes");
    let vk = VerifyingKey::from_bytes(&key_arr).expect("valid Ed25519 verifying key");

    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&signed.signature)
        .expect("signature must be valid base64");
    let sig = Signature::from_slice(&sig_bytes).expect("signature must be 64 bytes");

    vk.verify(signed.payload.as_bytes(), &sig)
        .expect("embedded policy signature MUST verify (re-run sign-policy.sh)");

    let doc: PolicyDocumentMin = serde_json::from_str(&signed.payload)
        .expect("inner payload must be a valid PolicyDocument JSON");
    assert!(
        doc.schema_version <= SUPPORTED_MAJOR,
        "embedded policy schema_version {} exceeds supported major {}",
        doc.schema_version,
        SUPPORTED_MAJOR
    );
}
