//! Build-time-baked LicenseSeat publishable credentials (Option A from
//! the 2026-04-19 community-default-onboarding plan, Task 14b).
//!
//! These are NON-SECRET. The publishable key is the `pk_*` LicenseSeat
//! issues for client embedding (analogous to a Stripe `pk_live_*`
//! publishable key). The product slug is the LicenseSeat project name.
//! Together they tell the SDK *which project* to authenticate against —
//! they do NOT grant any privilege beyond making activation /
//! validation calls. The privileged admin secret (`sk_*`) lives only in
//! the `spur-license-admin` crate and is never embedded in user-facing
//! binaries.
//!
//! Override at compile time via env vars set in CI / build scripts:
//!
//! ```text
//! SPUR_BUILD_LICENSESEAT_PUBLISHABLE_KEY=pk_live_...
//! SPUR_BUILD_LICENSESEAT_PRODUCT_SLUG=spur
//! cargo build --release -p spur-cli
//! ```
//!
//! Without overrides, the `DEFAULT_*` constants below are used (production
//! credentials for the SPUR LicenseSeat tenant). Rotation requires a code
//! change + rebuild + redistribution.

const DEFAULT_PUBLISHABLE_KEY: &str = "pk_live_CUgszLVauUc1HjY4sxYebadzqmL2oHQPC";
const DEFAULT_PRODUCT_SLUG: &str = "spur";

/// Returns `(publishable_key, product_slug)` for activating the
/// LicenseSeat provider in default builds (Option A path).
///
/// Prefers compile-time env-var overrides (`option_env!`) when set;
/// falls back to baked-in [`DEFAULT_PUBLISHABLE_KEY`] /
/// [`DEFAULT_PRODUCT_SLUG`].
///
/// Returns `Some(...)` whenever defaults are present, which is always
/// in this build configuration. The `Option` return type preserves the
/// shape originally specified in the 2026-04-19 plan and lets future
/// builds opt out of bake-in by clearing the defaults.
pub fn baked_credentials() -> Option<(&'static str, &'static str)> {
    let key = option_env!("SPUR_BUILD_LICENSESEAT_PUBLISHABLE_KEY")
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_PUBLISHABLE_KEY);
    let slug = option_env!("SPUR_BUILD_LICENSESEAT_PRODUCT_SLUG")
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_PRODUCT_SLUG);
    if key.is_empty() || slug.is_empty() {
        None
    } else {
        Some((key, slug))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baked_credentials_returns_some_in_default_build() {
        let creds = baked_credentials();
        assert!(creds.is_some(), "default build must bake in credentials");
        let (key, slug) = creds.unwrap();
        assert!(
            key.starts_with("pk_"),
            "publishable key must use pk_ prefix per LicenseSeat convention; got: {key}"
        );
        assert!(!slug.is_empty(), "product slug must not be empty");
    }
}
