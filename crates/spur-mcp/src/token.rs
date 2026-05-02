use base64ct::{Base64UrlUnpadded, Encoding};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorkerTokenPayload {
    pub d: String, // delegation_id
    pub b: String, // brain_session_id
    pub e: u64,    // expiry (unix seconds)
}

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("token is malformed")]
    Malformed,
    #[error("token signature does not verify")]
    BadSignature,
    #[error("token expired")]
    Expired,
}

pub fn encode_token(key: &[u8; 32], payload: &WorkerTokenPayload) -> anyhow::Result<String> {
    let payload_json = serde_json::to_vec(payload)?;
    let payload_b64 = Base64UrlUnpadded::encode_string(&payload_json);
    let mut mac = HmacSha256::new_from_slice(key).map_err(|e| anyhow::anyhow!(e))?;
    mac.update(payload_b64.as_bytes());
    let sig = mac.finalize().into_bytes();
    let sig_b64 = Base64UrlUnpadded::encode_string(&sig);
    Ok(format!("{payload_b64}.{sig_b64}"))
}

pub fn validate_token(
    key: &[u8; 32],
    token: &str,
    skew_tolerance_secs: u64,
) -> Result<WorkerTokenPayload, TokenError> {
    let (payload_b64, sig_b64) = token.split_once('.').ok_or(TokenError::Malformed)?;
    let sig = Base64UrlUnpadded::decode_vec(sig_b64).map_err(|_| TokenError::Malformed)?;

    let mut mac = HmacSha256::new_from_slice(key).map_err(|_| TokenError::BadSignature)?;
    mac.update(payload_b64.as_bytes());
    mac.verify_slice(&sig).map_err(|_| TokenError::BadSignature)?;

    let payload_bytes =
        Base64UrlUnpadded::decode_vec(payload_b64).map_err(|_| TokenError::Malformed)?;
    let payload: WorkerTokenPayload =
        serde_json::from_slice(&payload_bytes).map_err(|_| TokenError::Malformed)?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    if payload.e + skew_tolerance_secs < now {
        return Err(TokenError::Expired);
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn key() -> [u8; 32] {
        [42; 32]
    }

    fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    #[test]
    fn round_trip_valid_token() {
        let payload = WorkerTokenPayload {
            d: "abc-123".into(),
            b: "session-99".into(),
            e: now_unix() + 3600,
        };
        let token = encode_token(&key(), &payload).unwrap();
        let decoded = validate_token(&key(), &token, /*skew_tolerance=*/ 30).unwrap();
        assert_eq!(decoded.d, "abc-123");
        assert_eq!(decoded.b, "session-99");
    }

    #[test]
    fn rejects_expired_token() {
        let payload = WorkerTokenPayload {
            d: "abc-123".into(),
            b: "session-99".into(),
            e: now_unix() - 100, // 100s in the past
        };
        let token = encode_token(&key(), &payload).unwrap();
        let err = validate_token(&key(), &token, 30).unwrap_err();
        assert!(matches!(err, TokenError::Expired));
    }

    #[test]
    fn accepts_token_within_skew_tolerance() {
        let payload = WorkerTokenPayload {
            d: "abc".into(),
            b: "s".into(),
            e: now_unix() - 10, // 10s past
        };
        let token = encode_token(&key(), &payload).unwrap();
        validate_token(&key(), &token, 30).expect("within tolerance");
    }

    #[test]
    fn rejects_bad_signature() {
        let payload = WorkerTokenPayload {
            d: "abc".into(),
            b: "s".into(),
            e: now_unix() + 60,
        };
        let token = encode_token(&key(), &payload).unwrap();
        let other_key = [99u8; 32];
        let err = validate_token(&other_key, &token, 30).unwrap_err();
        assert!(matches!(err, TokenError::BadSignature));
    }

    #[test]
    fn rejects_malformed_token() {
        let err = validate_token(&key(), "not.a.token", 30).unwrap_err();
        assert!(matches!(err, TokenError::Malformed));
    }
}
