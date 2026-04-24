# Follow-up: Gate `test_support` module behind `cfg(test)` or a feature

**Discovered during:** Tranche 1 final code review (2026-04-24).
**Related commits:** `f09dffb` (Task 4 added items to the module).
**Scope:** ~5 LOC (attribute changes), plus one-time build verification.

## Defect

`crates/spur-core/src/orchestrator.rs` around line 5106 defines `pub mod test_support { ... }` with no `#[cfg(test)]` gate. `crates/spur-core/src/lib.rs` re-exports it unconditionally via `pub use orchestrator::test_support`. The module is `#[doc(hidden)]` so it doesn't appear in rustdoc, but it IS compiled into release artifacts — every shipped binary carries the test-only adapters.

This predates Tranche 1 (the module already existed), but Task 4 (`f09dffb`) expanded it with:
- `pub trait RetirableMcpServer` (mirror of the private production trait)
- `pub struct RetirableMcpServerAdapter<S>`
- `pub async fn call_shutdown_mcp_server<S>`
- `pub const MCP_SHUTDOWN_TIMEOUT_MS: u64`

All intended strictly for integration tests.

## Why this matters

- **Binary size:** small cost today, but the module will grow as Tranche 2 and later tranches add more test scaffolding.
- **Stability surface:** `pub` items on a library crate are part of its API contract. External consumers could accidentally depend on the mirror trait; future refactors that were "safe" because the original trait was private would become breaking.
- **Convention drift:** elsewhere in spur-core, test helpers use `#[cfg(test)]` inline modules. The `test_support` module is the only deviation.

## Fix shape

Option A (simplest): `#[cfg(test)]` gate on the module. Works for unit tests in the same crate but NOT for integration tests in `tests/*.rs` (those compile as separate crates without `cfg(test)` set on the library).

Option B (recommended): add a Cargo feature:

```toml
# crates/spur-core/Cargo.toml
[features]
test-support = []
```

```rust
// crates/spur-core/src/orchestrator.rs
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support { ... }

// crates/spur-core/src/lib.rs
#[cfg(any(test, feature = "test-support"))]
pub use orchestrator::test_support;
```

Then in `Cargo.toml` `[dev-dependencies]` entries for integration tests that need the helpers, or via `cargo test --features test-support`, the module is visible. Release builds without the feature don't compile it.

Option C: move `test_support` into its own crate `spur-core-testing` used only in `[dev-dependencies]`. More invasive; probably overkill for current scope.

## Acceptance

- `cargo build --release` produces a binary with no `test_support` symbols (verify via `nm` or `cargo-bloat`).
- `cargo test --workspace` still passes.
- Tranche 2 additions to the module (if any) follow the new gating pattern.

## Priority

Medium. Not blocking Tranche 1 merge per the final reviewer, but should land before Tranche 2 expands `test_support` further.

## Resolution

Resolved in commit `0818bea` (2026-04-24). Applied Option B from the original follow-up:
- `test-support` feature added to `crates/spur-core/Cargo.toml`.
- `pub mod test_support` gated with `#[cfg(any(test, feature = "test-support"))]`.
- Integration tests that depend on the module declared with `required-features`.

Verified via `cargo build --release` — the module is no longer compiled into release artifacts.
