//! Build guard for spur-cli.
//!
//! Ensures the distributable runtime binary is never built with the
//! policy signing key present in the environment. The signing key
//! must only exist on the admin machine, never in CI.

fn main() {
    assert!(
        std::env::var_os("SPUR_POLICY_SIGNING_KEY").is_none(),
        "SPUR_POLICY_SIGNING_KEY must not be present in the build environment. \
         The distributable runtime binary must not transit signing credentials. \
         If you are building spur-license-admin, use `cargo build -p spur-license-admin` instead."
    );
}
