# Cargo / Build / CI Ergonomics Review

Spec reviewed at `d9ba89c` from branch `review/spec-codex-2026-04-26`.

## Findings

1. **Rollout list for loopback-skip wiring is stale and can leave CI/sandbox failures behind.** At `d9ba89c`, `rg '\.start\(\)' crates/spur-mcp/tests` finds `rmcp_streamable_http.rs`, `server_start_pidfile.rs`, `persisted_authority_flip.rs`, and `reconciler_tick.rs`. The design table instead names `pidfile_single_brain.rs` and `block_timeout_continuation.rs`, neither of which calls `McpCallbackServer::start()` in this commit. If implementation follows the table rather than regenerating the grep result, sandboxed `cargo test -p spur-mcp` can still fail with loopback `EPERM` in the omitted files. Recommendation: make the implementation checklist authoritative from grep output and remove non-start files from the expected wiring list.

2. **`tests/common/mod.rs` is Cargo-safe as a step-1-only file.** The Cargo Book states that each top-level file under `tests/` is compiled as a separate integration-test crate, and shared code may live in `tests/common/mod.rs` and be imported with `mod common;` from each test file: <https://doc.rust-lang.org/cargo/reference/cargo-targets.html#integration-tests>. Because step 1 adds only `tests/common/mod.rs` and no crate root declares `mod common;`, Cargo/rustc will not compile that module, so it will not emit a `module is never used` warning. There is also no top-level `tests/common.rs` and no `[[test]]` entry in `crates/spur-mcp/Cargo.toml`, so it will not become an integration binary.

3. **Verification step 5 is valid.** Cargo documents that `cargo test [testname]` forwards the filter to libtest, and rustc documents that filters match substrings in the full test-function path; `--list` prints matching tests: <https://doc.rust-lang.org/cargo/commands/cargo-test.html>, <https://doc.rust-lang.org/rustc/tests/#filters>. Local probes confirmed the behavior: `cargo test -p spur-mcp mutation_scans_paginate_past_10k_issues -- --list` lists the current suffix match, and a nested-module probe listed `plan::labels::tests::constructors_produce_expected_strings`. The proposed `mutation_pagination::perf_regressions::mutation_scans_paginate_past_10k_issues` path should therefore be discoverable as claimed.

## CI Notes

`.github/workflows/` contains release, Python wheel, vendor-leak, and invariant-lint workflows. I found no workflow step invoking `cargo test -p spur-mcp`, no `--no-default-features` / `--all-features`, and no custom test filter that the rename or `tests/common/mod.rs` design would break.
