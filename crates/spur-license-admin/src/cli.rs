use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::str::FromStr;

/// String wrapper that redacts its contents in `Debug` output.
///
/// Used for the `LicenseSeat` `sk_*` secret key so it never leaks via
/// logged or panicked CLI structs.
#[derive(Clone)]
pub struct RedactedString(String);

impl std::fmt::Debug for RedactedString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RedactedString(\"REDACTED\")")
    }
}

impl FromStr for RedactedString {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

impl From<String> for RedactedString {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl PartialEq<&str> for RedactedString {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl RedactedString {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

#[derive(Parser, Debug)]
#[command(name = "spur-license-admin")]
#[command(about = "Admin CLI for LicenseSeat license management and SPUR policy signing")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Sign a SPUR policy document with an Ed25519 key
    SignPolicy {
        /// Path to the policy JSON file (raw `PolicyDocument` or existing `SignedPolicy`)
        input: PathBuf,

        /// Output path for the signed policy (prints to stdout if omitted)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Key ID to embed in the signature
        #[arg(short, long, default_value = "spur-policy-2026-04")]
        key_id: String,

        /// Path to the Ed25519 signing key (32 raw bytes or PKCS#8 PEM)
        #[arg(short, long, env = "SPUR_POLICY_SIGNING_KEY")]
        signing_key: PathBuf,
    },

    /// Manage `LicenseSeat` licenses
    License {
        #[command(subcommand)]
        action: LicenseAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum LicenseAction {
    /// Create a new `LicenseSeat` license
    Create {
        /// Plan key (e.g., pro, team, enterprise)
        #[arg(short, long)]
        plan: String,

        /// Customer email
        #[arg(short, long)]
        email: Option<String>,

        /// Number of seats
        #[arg(long)]
        seats: Option<u32>,

        /// `LicenseSeat` secret key (sk_*)
        #[arg(short, long, env = "SPUR_LICENSESEAT_SECRET_KEY")]
        secret_key: RedactedString,

        /// Product slug
        #[arg(long, env = "SPUR_LICENSESEAT_PRODUCT_SLUG")]
        product: String,
    },

    /// Revoke a `LicenseSeat` license
    Revoke {
        /// License key to revoke
        #[arg(short, long)]
        key: String,

        /// `LicenseSeat` secret key (sk_*)
        #[arg(short, long, env = "SPUR_LICENSESEAT_SECRET_KEY")]
        secret_key: RedactedString,

        /// Product slug
        #[arg(long, env = "SPUR_LICENSESEAT_PRODUCT_SLUG")]
        product: String,
    },

    /// List activations (seats) for a license
    Activations {
        /// License key
        #[arg(short, long)]
        key: String,

        /// `LicenseSeat` secret key (sk_*)
        #[arg(short, long, env = "SPUR_LICENSESEAT_SECRET_KEY")]
        secret_key: RedactedString,

        /// Product slug
        #[arg(long, env = "SPUR_LICENSESEAT_PRODUCT_SLUG")]
        product: String,
    },
}
