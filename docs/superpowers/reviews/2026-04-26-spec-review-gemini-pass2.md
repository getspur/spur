# Spec Review: spur-mcp perf-test relabel + loopback-bind sandbox skip

**Reviewer:** Gemini
**Date:** 2026-04-26

## 1. Macro Export & Path Hygiene (Clean)
The macro design using `#[macro_export]` alongside `mod common;` at the root of each integration test file is robust.
*   **Path Resolution:** Because each integration `.rs` file in `tests/` is compiled as a separate crate, `mod common;` puts the module at the root of that specific crate. Inside the macro, `$crate` hygienically resolves to the invoking crate. `$crate::common::loopback_bindable()` correctly expands to `crate::common::loopback_bindable()`.
*   **`.await` Hygiene:** Expanding an `.await` inside a macro invoked at the top of a `#[tokio::test]` `async fn` is perfectly valid. The early `return` evaluates hygienically and correctly terminates the enclosing test function.

## 2. `tokio::sync::OnceCell` Scope (Clean)
The `tokio` dependency configuration in `Cargo.toml` (`features = ["full", "test-util"]`) guarantees that the `tokio::sync` module is available.
*   `OnceCell::const_new()` has been a stable `const fn` since Tokio 1.21, allowing safe static initialization.
*   The concurrent initialization logic accurately ensures the async probe runs at most once per test binary without race conditions.

## 3. Named-Import Block in `perf_regressions` (Minor Defect)
The spec proposes an explicit block of named imports inside `mod perf_regressions {}` but misses a crucial symbol used by the test body:
*   **Missing Import:** `TempDir` (from `tempfile::TempDir`). Line 142 of the unchanged test body initializes the bead sandbox via `let dir = TempDir::new().expect("tempdir");`. If implemented verbatim, the test will fail to compile.
*   **Faulty Rationale:** The spec claims that `use super::*` "would silently miss the file-scope private helpers" because "Rust's glob import only re-exports pub items". **This is factually incorrect.** In Rust, a child module has visibility into its parent, and `use super::*;` inside a child imports *all* visible items from the parent, including private ones. The glob import works perfectly for this use case.

## 4. Other Considerations (Clean)
*   **Macro Return Shapes:** Providing a binary macro arm `($name:expr, $ret:expr)` that evaluates `return $ret;` handles the `Result<(), Box<dyn std::error::Error>>` use case cleanly via `Ok(())` inference.
*   **Unused Code Warnings:** Because `mod common;` is strictly added only to test files that touch the loopback listener, test binaries that do not need the probe won't trigger `dead_code` warnings for `loopback_bindable`.

## Verdict & Recommendation
**Verdict:** Minor defect (compilation error if followed verbatim).

**Recommendation:** Either add `TempDir` to the named import list in `mod perf_regressions`, or simplify the design by replacing the entire explicit import block with `use super::*;` since the spec's premise for avoiding it is flawed.
