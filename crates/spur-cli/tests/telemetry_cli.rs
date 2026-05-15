use assert_cmd::Command;
use serde::Deserialize;
use tempfile::tempdir;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
struct TelemetryToml {
    anonymous_id: Uuid,
    tier1_crash: bool,
    tier1_perf: bool,
    tier2_usage: bool,
}

fn read_telemetry_toml(home: &std::path::Path) -> TelemetryToml {
    let path = home.join(".spur").join("telemetry.toml");
    let body = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed reading {}: {e}", path.display()));
    toml::from_str(&body).expect("telemetry.toml should parse")
}

#[test]
fn telemetry_enable_disable_transitions_update_toml() {
    let home = tempdir().expect("home tempdir");

    Command::cargo_bin("spur")
        .expect("cargo bin")
        .env("HOME", home.path())
        .args(["telemetry", "disable", "all"])
        .assert()
        .success();

    let cfg = read_telemetry_toml(home.path());
    assert!(!cfg.tier1_crash);
    assert!(!cfg.tier1_perf);
    assert!(!cfg.tier2_usage);

    Command::cargo_bin("spur")
        .expect("cargo bin")
        .env("HOME", home.path())
        .args(["telemetry", "enable", "usage"])
        .assert()
        .success();

    let cfg = read_telemetry_toml(home.path());
    assert!(!cfg.tier1_crash);
    assert!(!cfg.tier1_perf);
    assert!(cfg.tier2_usage);

    Command::cargo_bin("spur")
        .expect("cargo bin")
        .env("HOME", home.path())
        .args(["telemetry", "enable", "all"])
        .assert()
        .success();

    let cfg = read_telemetry_toml(home.path());
    assert!(cfg.tier1_crash);
    assert!(cfg.tier1_perf);
    assert!(cfg.tier2_usage);
}

#[test]
fn telemetry_disable_crash_prints_notice_and_turns_off_only_crash() {
    let home = tempdir().expect("home tempdir");

    Command::cargo_bin("spur")
        .expect("cargo bin")
        .env("HOME", home.path())
        .args(["telemetry", "enable", "all"])
        .assert()
        .success();

    let out = Command::cargo_bin("spur")
        .expect("cargo bin")
        .env("HOME", home.path())
        .args(["telemetry", "disable", "crash"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8_lossy(&out);
    assert!(
        stdout.contains("does not delete existing crash files"),
        "stdout did not include expected notice: {stdout}"
    );

    let cfg = read_telemetry_toml(home.path());
    assert!(!cfg.tier1_crash);
    assert!(cfg.tier1_perf);
    assert!(cfg.tier2_usage);
}

#[test]
fn telemetry_reset_id_rotates_anonymous_id() {
    let home = tempdir().expect("home tempdir");

    Command::cargo_bin("spur")
        .expect("cargo bin")
        .env("HOME", home.path())
        .args(["telemetry", "enable", "all"])
        .assert()
        .success();

    let before = read_telemetry_toml(home.path()).anonymous_id;

    Command::cargo_bin("spur")
        .expect("cargo bin")
        .env("HOME", home.path())
        .args(["telemetry", "reset-id"])
        .assert()
        .success();

    let after = read_telemetry_toml(home.path()).anonymous_id;
    assert_ne!(before, after, "reset-id should rotate anonymous_id");
}

#[test]
fn telemetry_config_in_non_tty_prints_status_and_exits_zero() {
    let home = tempdir().expect("home tempdir");

    let out = Command::cargo_bin("spur")
        .expect("cargo bin")
        .env("HOME", home.path())
        .args(["telemetry", "config"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8_lossy(&out);
    assert!(
        stdout.contains("telemetry config:"),
        "expected status output in non-tty mode; got: {stdout}"
    );
}
