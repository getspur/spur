use std::process::Command;

#[test]
fn help_documents_environment_configuration_without_secret_values_or_arguments() {
    let output = Command::new(env!("CARGO_BIN_EXE_spur-context-auth-client"))
        .arg("--help")
        .output()
        .expect("help command starts");

    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("help is UTF-8");
    assert!(help.contains("SPUR_AUTH_CLIENT_SECRET"));
    assert!(help.contains("SPUR_AUTH_TOKEN_ENDPOINT"));
    assert!(help.contains(
        "No secret, token, authorization code, or PKCE verifier is accepted as an argument."
    ));
    assert!(!help.contains("SPUR_AUTH_CLIENT_SECRET="));
}

#[test]
fn credential_like_trailing_arguments_are_rejected_before_configuration_is_read() {
    let output = Command::new(env!("CARGO_BIN_EXE_spur-context-auth-client"))
        .args(["m2m-token", "--forbidden-credential"])
        .output()
        .expect("CLI starts");

    assert_eq!(output.status.code(), Some(2));
}
