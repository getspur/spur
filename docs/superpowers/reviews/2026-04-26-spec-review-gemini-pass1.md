# Review: spur-mcp perf-test relabel + loopback-bind sandbox skip

**Reviewer:** Gemini
**Date:** 2026-04-26
**Commit:** d9ba89c

## Summary of Findings

- **Macro Hygiene & Scope**: `#[macro_export]` combined with `mod common;` in an integration test root cleanly exposes `skip_if_no_loopback!`. `$crate::common::loopback_bindable` accurately resolves to the local module's function, caching the probe result safely once per binary. The macro expansion resolves flawlessly for both unary and binary arms.
- **Dependencies**: `tokio::sync::OnceCell::const_new()` works perfectly. `spur-mcp` already has `tokio` in its `dev-dependencies` with the `full` feature enabled. This covers the `sync` feature housing `OnceCell`. No `Cargo.toml` tweaks are needed.
- **Import Exhaustiveness**: The `mod perf_regressions` block in the spec is missing an import for `TempDir`. Because `use tempfile::TempDir;` was defined at the file scope, it needs to be either explicitly imported inside the sub-module (e.g., `use tempfile::TempDir;`) or added to the `use super::{...};` list for the test to compile.

## Detailed Review

### Macro Hygiene
The strategy of defining a `mod common` in integration test binaries to share code and macros is a standard pattern in Rust. The `#[macro_export]` attribute elevates the `skip_if_no_loopback!` macro to the root of the individual test crate. Because each test file in `tests/` acts as its own crate root, `mod common;` ensures the macro is available throughout the file.

The internal reference to `$crate::common::loopback_bindable` correctly expands to the `common` module within the specific test binary, pointing accurately to the `tokio::sync::OnceCell` cache mechanism. This ensures the loopback check occurs at most once per test binary rather than globally across all binaries, meeting the desired caching behavior described in the spec. Both the unary and binary return arms resolve correctly based on the return type of the caller.

### Tokio OnceCell
The spec correctly specifies `tokio::sync::OnceCell` as the appropriate async-friendly caching primitive. Reviewing `crates/spur-mcp/Cargo.toml` confirms that `tokio` is included in `dev-dependencies` with the `full` feature. The `sync` module, which contains `OnceCell`, is inherently part of the `full` feature flag, ensuring `tokio::sync::OnceCell::const_new()` will compile without any missing dependency issues or feature flag adjustments.

### Missing Import in Sub-module
The creation of the `mod perf_regressions` module introduces an inner scope. The named-import block (`use super::{...};`) accurately pulls most of the file-level dependencies. However, examining lines 138-259 of `crates/spur-mcp/tests/mutation_pagination.rs` shows `use tempfile::TempDir;` is declared at the file scope.

The named imports inside the sub-module omit this. Consequently, calling `TempDir::new()` within the test body inside `mod perf_regressions` will result in a compiler error since `TempDir` is not in scope.

To resolve this, add `use tempfile::TempDir;` directly inside `mod perf_regressions { ... }` or update the file-level imports to be pulled via `super::TempDir` (if `TempDir` stays at the file scope).
