use std::fs;
use std::path::Path;

use base64::Engine as _;
use ed25519_dalek::pkcs8::DecodePrivateKey as _;
use ed25519_dalek::Verifier as _;
use spur_license::policy::SignedPolicy;

/// Sign a `PolicyDocument` JSON file and write a `SignedPolicy` JSON file.
///
/// If the input already has a `SignedPolicy` wrapper, extracts the inner
/// payload and re-signs it. Accepts either a 32-byte raw seed file or a
/// PKCS#8 PEM-encoded Ed25519 private key.
pub fn run(
    input: &Path,
    output: Option<&Path>,
    key_id: &str,
    signing_key_path: &Path,
) -> anyhow::Result<()> {
    let input_raw =
        fs::read_to_string(input).map_err(|e| anyhow::anyhow!("failed to read input file: {e}"))?;

    // Detect whether the file already has a SignedPolicy wrapper or is raw
    let payload = if let Ok(wrapped) = serde_json::from_str::<SignedPolicy>(&input_raw) {
        wrapped.payload
    } else {
        input_raw.trim().to_owned()
    };

    let signing_key = load_signing_key(signing_key_path)?;

    let signed = crate::policy_sign::sign_policy(&payload, key_id, &signing_key);

    // Self-verify: decode the just-produced signature and re-verify against
    // the payload. Catches bit-flips and any future regression in the
    // signing pipeline before we write the artifact.
    let sig_bytes = base64::engine::general_purpose::STANDARD
        .decode(&signed.signature)
        .map_err(|e| anyhow::anyhow!("self-verify: signature is not valid base64: {e}"))?;
    let sig = ed25519_dalek::Signature::from_slice(&sig_bytes)
        .map_err(|e| anyhow::anyhow!("self-verify: signature is not 64 bytes: {e}"))?;
    if signing_key
        .verifying_key()
        .verify(payload.as_bytes(), &sig)
        .is_err()
    {
        anyhow::bail!("self-verify failed: produced signature does not verify against payload");
    }

    let json = serde_json::to_string_pretty(&signed)
        .map_err(|e| anyhow::anyhow!("failed to serialize signed policy: {e}"))?;

    if let Some(out) = output {
        fs::write(out, json).map_err(|e| anyhow::anyhow!("failed to write output file: {e}"))?;
    } else {
        println!("{json}");
    }

    Ok(())
}

/// Load an Ed25519 signing key from either a 32-byte raw seed file or a
/// PKCS#8 PEM file. Tries the 32-byte raw path first; on length mismatch
/// falls back to PEM.
fn load_signing_key(path: &Path) -> anyhow::Result<ed25519_dalek::SigningKey> {
    let key_bytes =
        fs::read(path).map_err(|e| anyhow::anyhow!("failed to read signing key: {e}"))?;

    if key_bytes.len() == 32 {
        let key_arr: [u8; 32] = key_bytes
            .try_into()
            .expect("length checked == 32 immediately above");
        return Ok(ed25519_dalek::SigningKey::from_bytes(&key_arr));
    }

    ed25519_dalek::SigningKey::read_pkcs8_pem_file(path)
        .map_err(|e| anyhow::anyhow!("signing key must be 32 raw bytes or a PKCS#8 PEM file: {e}"))
}
