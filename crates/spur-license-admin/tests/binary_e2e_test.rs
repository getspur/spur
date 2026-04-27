//! End-to-end tests for the spur-license-admin binary.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn bin_path() -> PathBuf {
    // Cargo sets CARGO_BIN_EXE_spur-license-admin when running tests
    std::env::var_os("CARGO_BIN_EXE_spur-license-admin")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // Fallback for manual test runs
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/spur-license-admin")
        })
}

fn test_signing_key_path(temp_dir: &std::path::Path) -> PathBuf {
    let seed = [0x42u8; 32];
    let path = temp_dir.join("test-key.raw");
    fs::write(&path, seed).expect("write test key");
    path
}

#[test]
fn binary_sign_policy_produces_valid_output() {
    let temp_dir =
        std::env::temp_dir().join(format!("spur-license-admin-e2e-{}", std::process::id()));
    fs::create_dir_all(&temp_dir).expect("create temp dir");

    let input_path = temp_dir.join("policy.json");
    let output_path = temp_dir.join("signed.json");
    let key_path = test_signing_key_path(&temp_dir);

    let payload = r#"{"schema_version":1,"issued_at":"2026-04-27T00:00:00Z","tier_policies":{}}"#;
    fs::write(&input_path, payload).expect("write input");

    let output = Command::new(bin_path())
        .arg("sign-policy")
        .arg(&input_path)
        .arg("-o")
        .arg(&output_path)
        .arg("-s")
        .arg(&key_path)
        .arg("-k")
        .arg("test-key")
        .output()
        .expect("failed to execute binary");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "binary should exit successfully. stderr: {stderr}"
    );

    let raw = fs::read_to_string(&output_path).expect("read output");
    let signed: spur_license::policy::SignedPolicy =
        serde_json::from_str(&raw).expect("output must be valid SignedPolicy");

    assert_eq!(signed.key_id, "test-key");
    assert_eq!(signed.payload, payload);

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
}
