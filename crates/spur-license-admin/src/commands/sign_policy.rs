use std::fs;
use std::path::Path;

use base64::Engine as _;
use ed25519_dalek::Signer;
use spur_license::policy::SignedPolicy;

/// Sign a PolicyDocument JSON file and write a SignedPolicy JSON file.
///
/// If the input already has a SignedPolicy wrapper, extracts the inner
/// payload and re-signs it.
pub async fn run(
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
        input_raw.trim().to_string()
    };

    // Load signing key (raw 32 bytes)
    let key_bytes = fs::read(signing_key_path)
        .map_err(|e| anyhow::anyhow!("failed to read signing key: {e}"))?;
    let key_arr: [u8; 32] = key_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("signing key must be exactly 32 bytes"))?;
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&key_arr);

    // Sign
    let signature = signing_key.sign(payload.as_bytes());
    let signature_b64 = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());

    let signed = SignedPolicy {
        payload,
        signature: signature_b64,
        key_id: key_id.to_string(),
    };

    let json = serde_json::to_string_pretty(&signed)
        .map_err(|e| anyhow::anyhow!("failed to serialize signed policy: {e}"))?;

    if let Some(out) = output {
        fs::write(out, json).map_err(|e| anyhow::anyhow!("failed to write output file: {e}"))?;
    } else {
        println!("{json}");
    }

    Ok(())
}
