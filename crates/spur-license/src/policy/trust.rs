//! Embedded Ed25519 trust map. Multi-key from V1 to enable rotation: ship a
//! new binary that adds the new key BEFORE retiring the old key on the
//! issuance side; ship a later binary that removes the old key.

use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::policy::{PolicyDocument, SignedPolicy, CODE_SUPPORTED_MAJOR};

#[derive(Debug, thiserror::Error)]
pub enum PolicyVerifyError {
    #[error("unknown signing key id: {0}")]
    UnknownKeyId(String),
    #[error("invalid base64 signature: {0}")]
    InvalidSignatureEncoding(String),
    #[error("signature did not verify against payload")]
    SignatureMismatch,
    #[error("policy payload is not valid JSON: {0}")]
    PayloadParse(String),
    #[error("policy schema_version {found} exceeds supported major {supported}")]
    UnsupportedSchemaVersion { found: u32, supported: u32 },
    #[error("policy expired at {0}")]
    Expired(chrono::DateTime<chrono::Utc>),
}

/// Returns the static, embedded trusted-keys map. Add new keys here BEFORE
/// rotating issuance; remove old keys in a later release after issuance has
/// migrated.
pub fn trusted_keys() -> &'static BTreeMap<&'static str, VerifyingKey> {
    static KEYS: OnceLock<BTreeMap<&'static str, VerifyingKey>> = OnceLock::new();
    KEYS.get_or_init(|| {
        let mut m = BTreeMap::new();
        let raw: &[u8] = include_bytes!("../../resources/keys/spur-policy-2026-04.pub");
        let key_bytes: [u8; 32] = raw
            .try_into()
            .expect("pubkey file must be exactly 32 bytes");
        let vk = VerifyingKey::from_bytes(&key_bytes).expect("valid Ed25519 verifying key");
        m.insert("spur-policy-2026-04", vk);
        m
    })
}

/// Verify a `SignedPolicy` against the trusted keys, parse the payload, and
/// enforce schema-version + expiry. Fails closed on every error.
pub fn verify_signed_policy(signed: &SignedPolicy) -> Result<PolicyDocument, PolicyVerifyError> {
    let key = trusted_keys()
        .get(signed.key_id.as_str())
        .ok_or_else(|| PolicyVerifyError::UnknownKeyId(signed.key_id.clone()))?;

    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&signed.signature)
        .map_err(|e| PolicyVerifyError::InvalidSignatureEncoding(e.to_string()))?;
    let sig = Signature::from_slice(&sig_bytes)
        .map_err(|e| PolicyVerifyError::InvalidSignatureEncoding(e.to_string()))?;

    key.verify(signed.payload.as_bytes(), &sig)
        .map_err(|_| PolicyVerifyError::SignatureMismatch)?;

    let doc: PolicyDocument = serde_json::from_str(&signed.payload)
        .map_err(|e| PolicyVerifyError::PayloadParse(e.to_string()))?;

    if doc.schema_version > CODE_SUPPORTED_MAJOR {
        return Err(PolicyVerifyError::UnsupportedSchemaVersion {
            found: doc.schema_version,
            supported: CODE_SUPPORTED_MAJOR,
        });
    }

    if let Some(exp) = doc.expires_at {
        if exp < chrono::Utc::now() {
            return Err(PolicyVerifyError::Expired(exp));
        }
    }

    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_keys_contains_2026_04_key() {
        let keys = trusted_keys();
        assert!(keys.contains_key("spur-policy-2026-04"));
    }

    #[test]
    fn unknown_key_id_is_rejected() {
        let signed = SignedPolicy {
            payload:
                r#"{"schema_version":1,"issued_at":"2026-04-19T00:00:00Z","tier_policies":{}}"#
                    .into(),
            signature: base64::engine::general_purpose::STANDARD.encode([0u8; 64]),
            key_id: "no-such-key".into(),
        };
        let err = verify_signed_policy(&signed).unwrap_err();
        assert!(matches!(err, PolicyVerifyError::UnknownKeyId(_)));
    }

    #[test]
    fn tampered_signature_is_rejected() {
        let signed = SignedPolicy {
            payload:
                r#"{"schema_version":1,"issued_at":"2026-04-19T00:00:00Z","tier_policies":{}}"#
                    .into(),
            signature: base64::engine::general_purpose::STANDARD.encode([0u8; 64]),
            key_id: "spur-policy-2026-04".into(),
        };
        let err = verify_signed_policy(&signed).unwrap_err();
        assert!(matches!(err, PolicyVerifyError::SignatureMismatch));
    }

    #[test]
    fn future_schema_version_is_rejected() {
        let signed = SignedPolicy {
            payload:
                r#"{"schema_version":99,"issued_at":"2026-04-19T00:00:00Z","tier_policies":{}}"#
                    .into(),
            signature: base64::engine::general_purpose::STANDARD.encode([0u8; 64]),
            key_id: "spur-policy-2026-04".into(),
        };
        let err = verify_signed_policy(&signed).unwrap_err();
        assert!(matches!(err, PolicyVerifyError::SignatureMismatch));
    }
}
