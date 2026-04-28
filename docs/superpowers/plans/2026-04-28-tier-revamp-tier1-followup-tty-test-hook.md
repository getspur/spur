# Tier Revamp Plan C — Tier 1 Follow-up: `SPUR_FORCE_TTY` Test Hook

**Status:** ✅ RESOLVED 2026-04-28 by Plan C Tier 2 Task 3 on
`feat/tier-revamp-c-tier2` (single commit titled
`test(spur-cli): C Tier 2 Task 3 — SPUR_FORCE_TTY hook +
CTA-shape binary smoke`). The `SPUR_FORCE_TTY=1` debug-only env
override + CTA-shape binary smoke have landed.

Originally filed 2026-04-28 from codex post-merge review of Tier 1
(commits `94a6ff9d`, `f28374ba`, `1c17732a`). **Priority at filing:**
Low — manual pty verification confirmed CTA dispatch works
end-to-end on real terminals; no user-facing bug.

## The gap

Tier 1's binary-level smoke
(`crates/spur-cli/tests/cli_core_gate_e2e.rs::
spur_exec_under_stripped_key_renders_typed_error_at_binary_boundary`)
spawns `spur` via `assert_cmd::Command`. `assert_cmd` does NOT
allocate a pty for the child process, so `std::io::stderr().is_terminal()`
returns `false` in the spawned binary — the TTY-gated CTA path
never fires. The smoke asserts only that the typed-error key name
reaches stderr (the plain `Error: {err:#}` path), which is a real
regression net for the gate wiring but NOT for the CTA renderer
dispatch.

Concretely, a future change that:
- Inverts the `is_terminal()` predicate (e.g. `if !is_terminal()`)
- Drops the entire `find_gate_error` branch and falls straight to
  the plain print
- Renames `format_upgrade_cta` and forgets to update `main.rs`
  (compile error, but only if the binary path is exercised — the
  unit tests in `spur-license` would NOT catch a `main.rs`-side
  rename gap)

…would all PASS the existing binary smoke. Only `spur-license`'s
unit tests cover the renderer's output shape.

Codex post-merge review verdict (verbatim):
> 🟡 Binary smoke only protects nonzero/key propagation, not actual
> TTY CTA dispatch; manual pty verification passed, but regression
> coverage remains manual.

## Proposed fix: `SPUR_FORCE_TTY=1` debug-only env hook

Add a debug-only env-var override to the TTY gate so binary tests
can force the CTA path without allocating a pty:

```rust
// crates/spur-cli/src/main.rs::render_top_level_error
fn render_top_level_error(err: &anyhow::Error) {
    if is_tty_or_forced() {
        if let Some(gate_err) = spur_license::upgrade_cta::find_gate_error(err) {
            eprint!(
                "{}",
                spur_license::upgrade_cta::format_upgrade_cta(gate_err)
            );
            return;
        }
    }
    eprintln!("Error: {err:#}");
}

fn is_tty_or_forced() -> bool {
    if std::io::stderr().is_terminal() {
        return true;
    }
    // Debug-only override for assert_cmd-based binary tests.
    // `#[cfg(debug_assertions)]` so it cannot leak into release.
    #[cfg(debug_assertions)]
    {
        if std::env::var("SPUR_FORCE_TTY").is_ok() {
            return true;
        }
    }
    false
}
```

Then strengthen the binary smoke to assert the CTA SHAPE under the
override:

```rust
#[test]
fn spur_exec_under_stripped_key_renders_full_cta_under_force_tty() {
    let assert = Command::cargo_bin("spur")
        .expect("spur binary builds")
        .env("SPUR_LICENSE_TEST_STRIP_KEYS", "cli_core_exec")
        .env("SPUR_FORCE_TTY", "1")
        .env_remove("SPUR_LICENSE_DEV_PLAN")
        .args(["exec", "--agent", "claude-code", "irrelevant-task"])
        .assert()
        .failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains("cli_core_exec"));
    assert!(stderr.contains("spur auth status"));
    assert!(stderr.contains("spur auth login --key"));
    assert!(stderr.contains("spur auth logout"));
}
```

## Why this is low priority

- The unit tests in
  `crates/spur-license/src/upgrade_cta.rs::tests` exhaustively
  cover the CTA renderer's output shape (5 tests; key, recovery
  affordances, chain-walk semantics).
- Manual pty verification (codex did one during post-merge review)
  confirmed the wired path produces the expected output on a real
  terminal.
- The TTY-gate decision is a single `if` statement; the bug surface
  is small.

## When to land

Bundle with **Tier 2** (TUI modal) work, since Tier 2 will:
- Re-test the same `find_gate_error` chain-walk from a new render
  surface (TUI Block widget instead of stderr).
- Likely want the same `SPUR_FORCE_TTY`-style hook to test the TUI
  modal under non-pty test runners.

If Tier 2 doesn't materialize within a quarter, land this hook
standalone in a small `test(spur-cli): close Tier 1 TTY CTA
testability gap` commit.

## Out of scope here

- Refactor `find_gate_error` / `format_upgrade_cta` API surface
  (codex flagged the `String` return as potentially too narrow for
  Tier 3; defer to Tier 3 — YAGNI for now).
- Per-key user-facing labels (Tier 2).
- TUI modal rendering (Tier 2).

## References

- Tier 1 plan + post-merge addendum:
  `docs/superpowers/plans/2026-04-28-tier-revamp-plan-c-tier1-cli-denial-cta.md`
- Codex post-merge review delegation:
  `9fee6d79-3145-4563-89ea-a32fcd4784d9`
- Gemini side-by-side review delegation:
  `4a5a67d4-d120-4340-bac7-068921940759`
