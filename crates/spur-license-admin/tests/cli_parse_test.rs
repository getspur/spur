//! Tests for CLI argument parsing.

use clap::Parser as _;

#[test]
fn parse_sign_policy_with_short_flags() {
    let cli = spur_license_admin::cli::Cli::try_parse_from([
        "spur-license-admin",
        "sign-policy",
        "/tmp/policy.json",
        "-o",
        "/tmp/signed.json",
        "-k",
        "spur-policy-2026-04",
        "-s",
        "/tmp/key.pem",
    ])
    .expect("should parse");

    if let spur_license_admin::cli::Commands::SignPolicy {
        input,
        output,
        key_id,
        signing_key,
    } = cli.command
    {
        assert_eq!(input, std::path::PathBuf::from("/tmp/policy.json"));
        assert_eq!(output, Some(std::path::PathBuf::from("/tmp/signed.json")));
        assert_eq!(key_id, "spur-policy-2026-04");
        assert_eq!(signing_key, std::path::PathBuf::from("/tmp/key.pem"));
    } else {
        panic!("expected SignPolicy command");
    }
}

#[test]
fn parse_sign_policy_uses_default_key_id() {
    let cli = spur_license_admin::cli::Cli::try_parse_from([
        "spur-license-admin",
        "sign-policy",
        "/tmp/policy.json",
        "-s",
        "/tmp/key.pem",
    ])
    .expect("should parse");

    if let spur_license_admin::cli::Commands::SignPolicy { key_id, .. } = cli.command {
        assert_eq!(key_id, "spur-policy-2026-04");
    } else {
        panic!("expected SignPolicy command");
    }
}

#[test]
fn parse_sign_policy_requires_signing_key() {
    let result = spur_license_admin::cli::Cli::try_parse_from([
        "spur-license-admin",
        "sign-policy",
        "/tmp/policy.json",
    ]);
    assert!(result.is_err(), "should fail without signing key");
}

#[test]
fn parse_license_create_with_all_args() {
    let cli = spur_license_admin::cli::Cli::try_parse_from([
        "spur-license-admin",
        "license",
        "create",
        "--plan",
        "pro",
        "--email",
        "user@example.com",
        "--seats",
        "5",
        "--secret-key",
        "sk_test_xxx",
        "--product",
        "my-product",
    ])
    .expect("should parse");

    if let spur_license_admin::cli::Commands::License {
        action:
            spur_license_admin::cli::LicenseAction::Create {
                plan,
                email,
                seats,
                secret_key,
                product,
            },
    } = cli.command
    {
        assert_eq!(plan, "pro");
        assert_eq!(email, Some("user@example.com".to_owned()));
        assert_eq!(seats, Some(5));
        assert_eq!(secret_key, "sk_test_xxx");
        assert_eq!(product, "my-product");
    } else {
        panic!("expected License Create command");
    }
}

#[test]
fn parsed_create_command_redacts_secret_key_in_debug_output() {
    let cli = spur_license_admin::cli::Cli::try_parse_from([
        "spur-license-admin",
        "license",
        "create",
        "--plan",
        "pro",
        "--secret-key",
        "sk_test_xxx",
        "--product",
        "my-product",
    ])
    .expect("should parse");

    let debug = format!("{cli:?}");
    assert!(
        !debug.contains("sk_test_xxx"),
        "Debug output must not leak secret key, but got: {debug}"
    );
    assert!(
        debug.contains("REDACTED"),
        "Debug output should mark the secret as REDACTED, got: {debug}"
    );
}

#[test]
fn parse_license_revoke() {
    let cli = spur_license_admin::cli::Cli::try_parse_from([
        "spur-license-admin",
        "license",
        "revoke",
        "--key",
        "TEST-KEY-1234",
        "--secret-key",
        "sk_test_xxx",
        "--product",
        "my-product",
    ])
    .expect("should parse");

    if let spur_license_admin::cli::Commands::License {
        action:
            spur_license_admin::cli::LicenseAction::Revoke {
                key,
                secret_key,
                product,
            },
    } = cli.command
    {
        assert_eq!(key, "TEST-KEY-1234");
        assert_eq!(secret_key, "sk_test_xxx");
        assert_eq!(product, "my-product");
    } else {
        panic!("expected License Revoke command");
    }
}

#[test]
fn parse_license_activations() {
    let cli = spur_license_admin::cli::Cli::try_parse_from([
        "spur-license-admin",
        "license",
        "activations",
        "--key",
        "TEST-KEY-1234",
        "--secret-key",
        "sk_test_xxx",
        "--product",
        "my-product",
    ])
    .expect("should parse");

    if let spur_license_admin::cli::Commands::License {
        action:
            spur_license_admin::cli::LicenseAction::Activations {
                key,
                secret_key,
                product,
            },
    } = cli.command
    {
        assert_eq!(key, "TEST-KEY-1234");
        assert_eq!(secret_key, "sk_test_xxx");
        assert_eq!(product, "my-product");
    } else {
        panic!("expected License Activations command");
    }
}
