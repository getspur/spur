//! Policy signing for SPUR tier entitlements.
//!
//! Replaces `scripts/sign-policy.sh` with a pure-Rust implementation
//! that runs entirely inside the admin binary.

use base64::Engine as _;
use ed25519_dalek::Signer as _;
use spur_license::policy::SignedPolicy;

/// Sign a canonical JSON policy payload with an Ed25519 key.
pub fn sign_policy(
    payload: &str,
    key_id: &str,
    signing_key: &ed25519_dalek::SigningKey,
) -> SignedPolicy {
    let signature = signing_key.sign(payload.as_bytes());
    let signature_b64 = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());

    SignedPolicy {
        payload: payload.to_owned(),
        signature: signature_b64,
        key_id: key_id.to_owned(),
    }
}
