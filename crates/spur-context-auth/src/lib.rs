//! Secure OAuth/OIDC and credential management for the SPUR context service.
//!
//! The crate deliberately separates human management sessions from personal
//! API keys. It owns browser-login protocol state, typed management requests,
//! and local secret storage, but has no dependency on the context-service
//! Lambda or on infrastructure source paths.
//!
//! API keys are imported as one canonical stdin line and remain redacted:
//!
//! ```
//! use secrecy::ExposeSecret;
//! use spur_context_auth::credentials::ApiKeyCredential;
//!
//! let key = ApiKeyCredential::parse_stdin(
//!     "spur_test_aaaaaaaaaaaaaaaaaaaaaaaaaa_bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
//! )?;
//! assert_eq!(key.public_id(), "aaaaaaaaaaaaaaaaaaaaaaaaaa");
//! assert_eq!(format!("{key:?}"), "ApiKeyCredential([REDACTED])");
//! // Exposure is explicit and should occur only at the HTTP header boundary.
//! assert!(key.secret().expose_secret().starts_with("spur_test_"));
//! # Ok::<(), spur_context_auth::credentials::CredentialError>(())
//! ```

pub mod credentials;
pub mod management;
pub mod oauth;
